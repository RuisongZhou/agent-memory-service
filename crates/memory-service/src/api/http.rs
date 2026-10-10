//! 接入层: local HTTP (`POST /v1/{接口名}`, JSON). Platform is taken from the connection
//! (`X-Memory-Platform`), never from the request body (2.2). `X-Memory-Tenant` selects the store
//! (evaluation only; the product runs a single store named `default`).
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;

use super::engine::Engine;
use super::types::*;
use super::{consolidate, indexer, ingest, manage, retrieve};

pub fn router(eng: Arc<Engine>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/{name}", post(api))
        .route("/admin/{name}", post(admin))
        .with_state(eng)
}

fn caller(h: &HeaderMap) -> Caller {
    let tenant = h
        .get("x-memory-tenant")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("default")
        .to_string();
    let platform = h
        .get("x-memory-platform")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("sdk")
        .to_string();
    Caller { tenant, platform }
}

fn err_response(e: ApiError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut body = json!({"code": e.code(), "message": e.to_string()});
    if let ApiError::NotCurrent(id) = &e {
        body["currentId"] = json!(id);
    }
    (status, Json(body)).into_response()
}

fn parse<T: DeserializeOwned>(v: Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| ApiError::InvalidArgument(e.to_string()))
}

async fn health(State(eng): State<Arc<Engine>>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "promptVariant": eng.cfg.prompt_variant,
        "embedModel": eng.models.embed_model_id,
        "rerankModel": eng.models.rerank_model_id,
        "modelBackend": eng.models.backend,
        "modelDevice": eng.models.device,
        "modelDtype": eng.models.dtype,
        "llm": eng.llm.usage_snapshot(),
        "tenants": eng.tenant_names().await,
        "tenantPromptVariants": eng.tenant_prompt_variants().await,
    }))
}

async fn api(
    State(eng): State<Arc<Engine>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let c = caller(&headers);
    let res: ApiResult<Value> = async {
        let t = eng.tenant(&c.tenant).await?;
        match name.as_str() {
            "add_memories" => ingest::add_memories(&eng, &t, &c, parse(body)?).await,
            "get_task" => ingest::get_task(&eng, &t, parse(body)?).await,
            "search_memories" => retrieve::search_memories(&eng, &t, &c, parse(body)?).await,
            "get_context" => retrieve::get_context(&eng, &t, &c, parse(body)?).await,
            "list_turns" => retrieve::list_turns(&eng, &t, &c, parse(body)?).await,
            "get_memory" => retrieve::get_memory(&eng, &t, parse(body)?).await,
            "correct_memory" => manage::correct_memory(&eng, &t, &c, parse(body)?).await,
            "delete_memory" => manage::delete_memory(&eng, &t, parse(body)?).await,
            "feedback" => manage::feedback(&eng, &t, parse(body)?).await,
            other => Err(ApiError::NotFound(format!("unknown interface {other}"))),
        }
    }
    .await;
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn admin(
    State(eng): State<Arc<Engine>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let c = caller(&headers);
    let body = body.map(|b| b.0).unwrap_or(json!({}));
    let res: ApiResult<Value> = async {
        let t = eng.tenant(&c.tenant).await?;
        match name.as_str() {
            "run_jobs" => consolidate::run_jobs(&eng, &t, &c, body).await,
            "wait_idle" => ingest::wait_idle(&eng, &t).await,
            "stats" => manage::stats(&eng, &t).await,
            "export" => manage::export(&eng, &t, body).await,
            "seed" if std::env::var("MEMORY_MOCK_MODELS").as_deref() == Ok("1") => {
                manage::seed(&eng, &t, body).await
            }
            "reindex" => {
                let v = body.get("vector").and_then(|x| x.as_bool()).unwrap_or(true);
                let f = body
                    .get("fulltext")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(true);
                indexer::schedule_rebuild(&t, v, f).await?;
                Ok(json!({"scheduled": true}))
            }
            other => Err(ApiError::NotFound(format!("unknown admin action {other}"))),
        }
    }
    .await;
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

/// OpenAI-compatible embeddings endpoint backed by the daemon's local embedder (lets baselines such as mem0
/// use exactly the same embedding model).
async fn embeddings(State(eng): State<Arc<Engine>>, Json(body): Json<Value>) -> Response {
    let inputs: Vec<String> = match body.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) if !a.is_empty() && a.len() <= 128 => {
            let Some(inputs) = a
                .iter()
                .map(|x| x.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            else {
                return err_response(ApiError::InvalidArgument(
                    "every embedding input must be a string".into(),
                ));
            };
            inputs
        }
        _ => {
            return err_response(ApiError::InvalidArgument(
                "input must be a string or a nonempty array of at most 128 strings".into(),
            ))
        }
    };
    match eng.models.embed(&inputs).await {
        Ok(vecs) => {
            let data: Vec<Value> = vecs
                .iter()
                .enumerate()
                .map(|(i, v)| json!({"object": "embedding", "index": i, "embedding": v.as_slice()}))
                .collect();
            let tokens: usize = inputs.iter().map(|s| s.len() / 4).sum();
            Json(json!({
                "object": "list",
                "data": data,
                "model": eng.models.embed_model_id,
                "usage": {"prompt_tokens": tokens, "total_tokens": tokens}
            }))
            .into_response()
        }
        Err(e) => err_response(ApiError::Internal(e.to_string())),
    }
}
