//! 接入层: local HTTP (`POST /v1/{接口名}`, JSON). Platform is taken from the connection
//! (`X-Memory-Platform`), never from the request body (2.2).
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;

use memory_domain::types::*;
use memory_engine::app::management;
use memory_engine::app::{consolidation, import, ingest};
use memory_engine::retrieval;
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::indexer;

/// The daemon's routes. `port` is the one its listener is bound to, the only one clients may address it by;
/// `ui_dir` is the web UI's build output served under `/ui` (`--ui-dir`), `installer` what its platform page runs
/// (`--installer`). The loopback check covers every route.
pub fn router(
    eng: Arc<Engine>,
    port: u16,
    ui_dir: Option<&std::path::Path>,
    installer: Option<std::path::PathBuf>,
) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/{name}", post(api))
        .route("/admin/{name}", post(admin))
        .merge(super::ui::router(ui_dir))
        .merge(super::platforms::router(installer))
        .with_state(eng)
        .layer(middleware::from_fn_with_state(port, loopback_only))
}

/// The daemon has no authentication; loopback is its boundary. A browser page on another origin can reach it
/// with a "simple" request (`POST /admin/reindex` without body or content type needs no preflight) or, by
/// DNS rebinding, under a name of its own that resolves to loopback. Both are refused here, before any route runs.
async fn loopback_only(State(port): State<u16>, req: Request, next: Next) -> Response {
    // Only the headers and the URI are borrowed, never the body, which is not `Sync`: the future stays `Send`.
    let (headers, uri) = (req.headers(), req.uri());
    // A header that is not visible ASCII reads as empty, which no check accepts: never as an absent header.
    let header = |name: &str| headers.get(name).map(|v| v.to_str().unwrap_or_default());
    // An HTTP/2 request has no `Host` header: hyper reports its `:authority` in the URI.
    let host = header("host").or_else(|| uri.authority().map(|a| a.as_str()));
    let refused = refusal(host, header("origin"), header("sec-fetch-site"), port);
    match refused {
        Some(why) => err_response(ApiError::Forbidden(why.into())),
        None => next.run(req).await,
    }
}

/// Why a request must not be served, or `None`: it is not addressed to the daemon as `127.0.0.1`, `localhost` or
/// `[::1]` on its port, it comes from another origin, or the browser marks it cross-site. Clients on the machine
/// (curl, the MCP bridge, the hooks, the installer) send no `Origin` or `Sec-Fetch-Site` and pass.
fn refusal(
    host: Option<&str>,
    origin: Option<&str>,
    fetch_site: Option<&str>,
    port: u16,
) -> Option<&'static str> {
    let ours = |authority: &str| {
        authority.rsplit_once(':').is_some_and(|(name, p)| {
            p.parse() == Ok(port)
                && ["127.0.0.1", "localhost", "[::1]"]
                    .iter()
                    .any(|loopback| loopback.eq_ignore_ascii_case(name))
        })
    };
    if !host.is_some_and(ours) {
        Some("Host must be 127.0.0.1, localhost or [::1] with the daemon's port")
    } else if origin.is_some_and(|o| !o.strip_prefix("http://").is_some_and(ours)) {
        Some("Origin must be the daemon's own loopback address")
    } else if fetch_site == Some("cross-site") {
        Some("cross-site requests are refused")
    } else {
        None
    }
}

fn caller(h: &HeaderMap) -> ApiResult<Caller> {
    let platform = h
        .get("x-memory-platform")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("sdk")
        .to_string();
    let client = match h.get("x-memory-client") {
        None => ClientKind::Http,
        Some(v) => v.to_str().ok().and_then(ClientKind::parse).ok_or_else(|| {
            ApiError::InvalidArgument("X-Memory-Client must be hook, mcp or http".into())
        })?,
    };
    Ok(Caller { platform, client })
}

pub(super) fn err_response(e: ApiError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).expect("ApiError status codes are valid");
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
        "version": env!("CARGO_PKG_VERSION"),
        "promptVariant": eng.cfg.prompt_variant,
        "embedModel": eng.models.embed_model_id,
        "rerankModel": eng.models.rerank_model_id,
        "modelBackend": eng.models.backend,
        "modelDevice": eng.models.device,
        "modelDtype": eng.models.dtype,
        "llm": eng.llm.usage_snapshot(),
        "memoryStrategy": eng.strategy(),
    }))
}

/// One public interface call (`POST /v1/{name}`); the in-process dialogue uploader shares it.
pub async fn call(eng: &Arc<Engine>, c: &Caller, name: &str, body: Value) -> ApiResult<Value> {
    let t = eng.store().await?;
    match name {
        "add_memories" => ingest::add_memories(eng, &t, c, parse(body)?).await,
        "get_task" => ingest::get_task(eng, &t, parse(body)?).await,
        "search_memories" => eng.search_memories(&t, c, parse(body)?).await,
        "get_context" => retrieval::get_context(eng, &t, c, parse(body)?).await,
        "list_turns" => eng.list_turns(&t, c, parse(body)?).await,
        "get_memory" => retrieval::get_memory(eng, &t, parse(body)?).await,
        "correct_memory" => management::correct_memory(eng, &t, c, parse(body)?).await,
        "delete_memory" => management::delete_memory(eng, &t, parse(body)?).await,
        "feedback" => management::feedback(eng, &t, parse(body)?).await,
        "resolve_uncertainty" => management::resolve_uncertainty(&t, c, parse(body)?).await,
        other => Err(ApiError::NotFound(format!("unknown interface {other}"))),
    }
}

async fn api(
    State(eng): State<Arc<Engine>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let res = async { call(&eng, &caller(&headers)?, &name, body).await }.await;
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn admin(
    State(eng): State<Arc<Engine>>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or(json!({}));
    let res: ApiResult<Value> = async {
        let t = eng.store().await?;
        match name.as_str() {
            "wait_idle" => ingest::wait_idle(&eng, &t).await,
            "stats" => management::stats(&eng, &t).await,
            "export" => management::export(&eng, &t, body).await,
            "consolidate" => consolidation::consolidate(&eng, &t, parse(body)?).await,
            "consolidation_apply" => consolidation::apply(&eng, &t, parse(body)?).await,
            "consolidation_rollback" => consolidation::rollback(&eng, &t, parse(body)?).await,
            "consolidation_runs" => consolidation::list_runs(&t, parse(body)?).await,
            "consolidation_run" => consolidation::get_run(&t, parse(body)?).await,
            "import_propose" => import::propose(&eng, &t, parse(body)?).await,
            "import_apply" => import::apply(&t, parse(body)?).await,
            "import_rollback" => import::rollback(&eng, &t, parse(body)?).await,
            "import_run" => import::get_run(&t, parse(body)?).await,
            "import_runs" => import::list_runs(&t, parse(body)?).await,
            "reindex" => {
                indexer::schedule_rebuild(&t).await?;
                Ok(json!({"scheduled": true}))
            }
            "list_memories" => management::list_memories(&t, parse(body)?).await,
            "memory_facets" => management::memory_facets(&t).await,
            "memory_lineage" => management::memory_lineage(&t, parse(body)?).await,
            "list_entities" => management::list_entities(&eng, &t, parse(body)?).await,
            "entity" => management::entity(&eng, &t, parse(body)?).await,
            "memory_entities" => management::memory_entities(&eng, &t, parse(body)?).await,
            "list_uncertainties" => management::list_uncertainties(&t, parse(body)?).await,
            "list_retrievals" => management::list_retrievals(&t, parse(body)?).await,
            "list_requests" => management::list_requests(&t, parse(body)?).await,
            "request" => management::request(&t, parse(body)?).await,
            "list_model_responses" => management::list_model_responses(&t, parse(body)?).await,
            "platform_activity" => management::platform_activity(&t).await,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine;
    use axum::http::HeaderValue;
    use memory_config::ServiceConfig;

    /// A browser page on another origin cannot use the daemon: its `no-cors` fetch to `127.0.0.1` and a name it
    /// rebinds to loopback are refused, while a client that sends no `Origin` or `Sec-Fetch-Site` and addresses
    /// the daemon as loopback is served, over HTTP/1.1 and over HTTP/2 (which has no `Host` header). A call naming
    /// a client the daemon does not know (`X-Memory-Client`) is refused.
    #[tokio::test]
    async fn the_daemon_refuses_what_a_page_on_another_origin_can_send() {
        // Each header on its own, the other two being a loopback client's, for a daemon on port 8765.
        let served = |host: Option<&str>, origin: Option<&str>, site: Option<&str>| {
            refusal(host, origin, site, 8765).is_none()
        };
        let ours = Some("127.0.0.1:8765");
        for host in ["127.0.0.1:8765", "LOCALHOST:8765", "[::1]:8765"] {
            assert!(served(Some(host), None, None), "{host}");
        }
        for host in [
            None,
            Some("evil.example:8765"),
            Some("127.0.0.1:8766"),
            Some("127.0.0.1"),
        ] {
            assert!(!served(host, None, None), "{host:?}");
        }
        for origin in ["http://localhost:8765", "http://[::1]:8765"] {
            assert!(served(ours, Some(origin), None), "{origin}");
        }
        for origin in [
            "null",
            "http://evil.example",
            "http://localhost:3000",
            "https://127.0.0.1:8765",
        ] {
            assert!(!served(ours, Some(origin), None), "{origin}");
        }
        for site in ["same-origin", "none"] {
            assert!(served(ours, None, Some(site)), "{site}");
        }
        assert!(!served(ours, None, Some("cross-site")));

        // The router on the port its listener got, as the daemon serves it.
        let data_dir = std::env::temp_dir().join(memory_rules::util::new_id("memory-http-"));
        let cfg = ServiceConfig {
            data_dir,
            ..Default::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = router(engine(cfg), port, None, None);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let daemon = format!("http://127.0.0.1:{port}");
        let rebound = format!("evil.example:{port}");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let requests: [(&[(&str, &str)], bool); 6] = [
            (&[], true),
            (
                &[("origin", &daemon), ("sec-fetch-site", "same-origin")],
                true,
            ),
            (&[("host", &rebound)], false),
            (&[("origin", "http://evil.example")], false),
            // Not visible ASCII: an Origin that is present, never one that is absent.
            (&[("origin", "http://é.example")], false),
            (&[("sec-fetch-site", "cross-site")], false),
        ];
        for (headers, served) in requests {
            // What a refused page sends is the attack: `POST /admin/reindex` with no body or content type.
            let request = if served {
                client.get(format!("{daemon}/health"))
            } else {
                client.post(format!("{daemon}/admin/reindex"))
            };
            let request = headers.iter().fold(request, |request, (name, value)| {
                request.header(*name, HeaderValue::from_bytes(value.as_bytes()).unwrap())
            });
            let response = request.send().await.unwrap();
            let status = response.status();
            let body: Value = response.json().await.unwrap();
            if served {
                assert_eq!(status, StatusCode::OK, "{headers:?}");
                assert_eq!(body["ok"], true);
            } else {
                assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
                assert_eq!(body["code"], "FORBIDDEN");
            }
        }

        let h2 = reqwest::Client::builder()
            .no_proxy()
            .http2_prior_knowledge()
            .build()
            .unwrap();
        let response = h2.get(format!("{daemon}/health")).send().await.unwrap();
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        assert_eq!(response.status(), StatusCode::OK);

        // A client the daemon does not know is refused before the call runs.
        let response = client
            .post(format!("{daemon}/v1/get_context"))
            .header("x-memory-client", "robot")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = response.json().await.unwrap();
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("X-Memory-Client"));
    }
}
