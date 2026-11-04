//! The web UI's model settings page (docs/design/model-settings.md): `/admin/model_settings` shows where the
//! generation, embedding and rerank models are and where each setting comes from, `/admin/model_settings_test`
//! tries proposed settings against the real endpoints, and `/admin/model_settings_apply` tries them, writes them to
//! the daemon's `--env-file` and restarts the daemon with them. API keys go in and are never sent back.
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use memory_config::{
    ModelProvider, ServiceConfig, DEFAULT_API_EMBED_MODEL, DEFAULT_API_RERANK_MODEL,
};
use memory_domain::types::{ApiError, ApiResult};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::models::{is_loopback, ModelHub};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::http::err_response;
use crate::envfile;
use crate::settings::{self, Launch};

/// Longest a test waits for the retrieval models: a llama-server that is still loading gets this long.
const RETRIEVAL_WAIT: Duration = Duration::from_secs(30);
/// Longest a test waits for the generation endpoint's model list.
const GENERATION_WAIT: Duration = Duration::from_secs(15);
/// Time the answer to a save has to reach the page before the daemon restarts.
const RESTART_DELAY: Duration = Duration::from_millis(500);

/// One setting: its field in the requests, its env-file key and the `serve` flag that overrides it.
struct Field {
    name: &'static str,
    key: &'static str,
    flag: Option<&'static str>,
}

const fn field(name: &'static str, key: &'static str, flag: Option<&'static str>) -> Field {
    Field { name, key, flag }
}

const LLM_BASE_URL: Field = field("llmBaseUrl", "QWEN_BASE_URL", Some("--llm-base-url"));
const LLM_MODEL: Field = field("llmModel", "LLM_MODEL", Some("--llm-model"));
const LLM_KEY: Field = field("llmKey", "QWEN_API_KEY", None);
const MODEL_PROVIDER: Field = field(
    "modelProvider",
    "MEMORY_MODEL_PROVIDER",
    Some("--model-provider"),
);
const EMBED_URL: Field = field("embedUrl", "MEMORY_EMBED_URL", Some("--embed-url"));
const RERANK_URL: Field = field("rerankUrl", "MEMORY_RERANK_URL", Some("--rerank-url"));
const EMBED_MODEL: Field = field("embedModel", "MEMORY_EMBED_MODEL", Some("--embed-model"));
const RERANK_MODEL: Field = field("rerankModel", "MEMORY_RERANK_MODEL", Some("--rerank-model"));
const MODEL_KEY: Field = field("modelKey", "MEMORY_MODEL_API_KEY", None);

/// The settings that are text, in the order the page shows them; the two keys are separate.
const TEXT: [&Field; 7] = [
    &LLM_BASE_URL,
    &LLM_MODEL,
    &MODEL_PROVIDER,
    &EMBED_URL,
    &RERANK_URL,
    &EMBED_MODEL,
    &RERANK_MODEL,
];

struct Ctx {
    eng: Arc<Engine>,
    launch: Launch,
    /// A save is restarting the daemon: no other save until it has.
    restarting: AtomicBool,
}

pub fn router<S: Clone + Send + Sync + 'static>(eng: Arc<Engine>, launch: Launch) -> Router<S> {
    Router::new()
        .route("/admin/model_settings", post(show))
        .route("/admin/model_settings_test", post(test))
        .route("/admin/model_settings_apply", post(apply))
        .with_state(Arc::new(Ctx {
            eng,
            launch,
            restarting: AtomicBool::new(false),
        }))
}

/// Where a setting's value comes from. A flag or an environment variable outranks the env file, so the page
/// cannot change those.
fn source(f: &Field, launch: &Launch) -> &'static str {
    if f.flag.is_some_and(|flag| launch.has_flag(flag)) {
        "flag"
    } else if launch.from_file.contains(&f.key) {
        "file"
    } else if env(f.key).is_some() {
        "environment"
    } else {
        "default"
    }
}

fn locked(source: &str) -> bool {
    matches!(source, "flag" | "environment")
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// What the daemon runs with for `f`, after defaults.
fn effective(f: &Field, cfg: &ServiceConfig) -> String {
    match f.name {
        "llmBaseUrl" => cfg.llm_base_url.clone(),
        "llmModel" => cfg.llm_model.clone(),
        "modelProvider" => provider_name(cfg.model_provider).into(),
        "embedUrl" => cfg.embed_url().into(),
        "rerankUrl" => cfg.rerank_url().into(),
        "embedModel" => cfg.embed_model.clone(),
        "rerankModel" => cfg.rerank_model.clone(),
        other => unreachable!("{other} is not a text setting"),
    }
}

/// What `f` is set to: a flag's value as it took effect, else the variable ("" when unset, the default applies).
fn configured(f: &Field, ctx: &Ctx) -> String {
    if source(f, &ctx.launch) == "flag" {
        effective(f, &ctx.eng.cfg)
    } else {
        env(f.key).unwrap_or_default()
    }
}

fn provider_name(p: ModelProvider) -> &'static str {
    match p {
        ModelProvider::Api => "api",
        ModelProvider::SelfHosted => "self-hosted",
    }
}

fn mock() -> bool {
    std::env::var("MEMORY_MOCK_MODELS").is_ok_and(|v| v == "1")
}

fn editable(launch: &Launch) -> bool {
    cfg!(unix) && launch.env_file.is_some()
}

async fn show(State(ctx): State<Arc<Ctx>>) -> Json<Value> {
    let (eng, launch) = (&ctx.eng, &ctx.launch);
    let mut fields = Map::new();
    for f in TEXT {
        fields.insert(
            f.name.into(),
            json!({"value": configured(f, &ctx), "effective": effective(f, &eng.cfg), "source": source(f, launch)}),
        );
    }
    for f in [&LLM_KEY, &MODEL_KEY] {
        fields.insert(
            f.name.into(),
            json!({"set": env(f.key).is_some(), "source": source(f, launch)}),
        );
    }
    let env_file = launch.env_file.as_deref();
    Json(json!({
        "editable": editable(launch),
        "envFile": env_file.map(|p| p.display().to_string()),
        "mock": mock(),
        "rerank": eng.cfg.use_reranker && eng.cfg.mem0_rerank,
        "fields": fields,
        "models": {
            "backend": eng.models.backend,
            "embedModel": eng.models.embed_model_id,
            "rerankModel": eng.models.rerank_model_id,
            "dim": eng.models.dim,
        },
        "rolledBack": env_file.and_then(settings::rolled_back),
    }))
}

/// Proposed settings. Each text field is what to set ("" for a URL or model selects the default); a key that is
/// absent or "" stays as it is.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Proposal {
    llm_base_url: String,
    llm_model: String,
    model_provider: String,
    #[serde(default)]
    embed_url: String,
    #[serde(default)]
    rerank_url: String,
    #[serde(default)]
    embed_model: String,
    #[serde(default)]
    rerank_model: String,
    #[serde(default)]
    llm_key: Option<String>,
    #[serde(default)]
    model_key: Option<String>,
}

impl Proposal {
    fn text(&self, f: &Field) -> &str {
        match f.name {
            "llmBaseUrl" => &self.llm_base_url,
            "llmModel" => &self.llm_model,
            "modelProvider" => &self.model_provider,
            "embedUrl" => &self.embed_url,
            "rerankUrl" => &self.rerank_url,
            "embedModel" => &self.embed_model,
            "rerankModel" => &self.rerank_model,
            other => unreachable!("{other} is not a text setting"),
        }
    }
}

/// Checked settings: the configuration they make, the keys to try it with and the env-file lines to write.
struct Plan {
    cfg: ServiceConfig,
    llm_key: String,
    model_key: String,
    values: Vec<(&'static str, String)>,
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::InvalidArgument(message.into())
}

/// A value the env file can hold as written: one line, no surrounding space or quotes, no control characters.
fn check_value(name: &str, value: &str, max: usize) -> ApiResult<()> {
    let quoted = |c: char| c == '"' || c == '\'';
    if value.len() > max
        || value.chars().any(char::is_control)
        || value.trim() != value
        || value.starts_with(quoted)
        || value.ends_with(quoted)
    {
        return Err(invalid(format!(
            "{name}: one line of at most {max} bytes, without surrounding spaces or quotes"
        )));
    }
    Ok(())
}

fn check_url(name: &str, url: &str) -> ApiResult<()> {
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Ok(()),
        _ => Err(invalid(format!("{name} must be an http(s) URL"))),
    }
}

/// Where a request to `url` goes: scheme, host and port. A key set for one origin is not sent to another.
fn origin(url: &str) -> Option<(String, String, u16)> {
    let u = reqwest::Url::parse(url).ok()?;
    Some((
        u.scheme().into(),
        u.host_str()?.to_ascii_lowercase(),
        u.port_or_known_default()?,
    ))
}

fn plan(p: &Proposal, ctx: &Ctx) -> ApiResult<Plan> {
    let old = &ctx.eng.cfg;
    for f in TEXT {
        check_value(f.name, p.text(f), 2048)?;
    }
    let given = |key: &Option<String>| key.clone().filter(|k| !k.is_empty());
    let (llm_key, model_key) = (given(&p.llm_key), given(&p.model_key));
    for (f, key) in [(&LLM_KEY, &llm_key), (&MODEL_KEY, &model_key)] {
        if let Some(key) = key {
            check_value(f.name, key, 512)?;
            if key.contains(char::is_whitespace) {
                return Err(invalid(format!("{} cannot contain spaces", f.name)));
            }
            if locked(source(f, &ctx.launch)) {
                return Err(invalid(format!(
                    "{} is set by the daemon's environment ({}); change it there",
                    f.name, f.key
                )));
            }
        }
    }
    for f in TEXT {
        let source = source(f, &ctx.launch);
        if locked(source) && p.text(f) != configured(f, ctx) {
            let by = match (source, f.flag) {
                ("flag", Some(flag)) => format!("the command-line flag {flag}"),
                _ => format!("the daemon's environment ({})", f.key),
            };
            return Err(invalid(format!(
                "{} is set by {by}; change it there",
                f.name
            )));
        }
    }

    check_url("llmBaseUrl", &p.llm_base_url)?;
    if p.llm_model.is_empty() {
        return Err(invalid("llmModel must name the generation model"));
    }
    let provider: ModelProvider = serde_json::from_value(json!(p.model_provider))
        .map_err(|_| invalid("modelProvider must be api or self-hosted"))?;
    for (name, url) in [("embedUrl", &p.embed_url), ("rerankUrl", &p.rerank_url)] {
        if !url.is_empty() {
            check_url(name, url)?;
        }
    }
    let or_none = |v: &str| (!v.is_empty()).then(|| v.to_string());
    let cfg = ServiceConfig {
        llm_base_url: p.llm_base_url.clone(),
        llm_model: p.llm_model.clone(),
        model_provider: provider,
        embed_url: or_none(&p.embed_url),
        rerank_url: or_none(&p.rerank_url),
        embed_model: or_none(&p.embed_model).unwrap_or_else(|| DEFAULT_API_EMBED_MODEL.into()),
        rerank_model: or_none(&p.rerank_model).unwrap_or_else(|| DEFAULT_API_RERANK_MODEL.into()),
        ..old.clone()
    };
    cfg.validate().map_err(|e| invalid(format!("{e:#}")))?;

    // A key already set is never sent to a host it was not set for: moving an endpoint to another origin
    // needs its key again. The retrieval models use their own key, else the generation key.
    let (old_llm, old_model) = (env(LLM_KEY.key), env(MODEL_KEY.key));
    if origin(&cfg.llm_base_url) != origin(&old.llm_base_url)
        && llm_key.is_none()
        && old_llm.is_some()
    {
        return Err(invalid(
            "the generation endpoint moves to another host: enter its API key again (the saved key is not sent there)",
        ));
    }
    let moved = origin(cfg.embed_url()) != origin(old.embed_url())
        || origin(cfg.rerank_url()) != origin(old.rerank_url());
    let reused = old_model.is_some() || (llm_key.is_none() && old_llm.is_some());
    if provider == ModelProvider::Api && moved && model_key.is_none() && reused {
        return Err(invalid(
            "the embedding or rerank endpoint moves to another host: enter the model API key again (the saved key is not sent there)",
        ));
    }

    let llm_effective = llm_key.clone().or(old_llm).unwrap_or_default();
    let model_effective = model_key
        .clone()
        .or(old_model)
        .unwrap_or_else(|| llm_effective.clone());
    let mut values: Vec<(&'static str, String)> = TEXT
        .iter()
        .filter(|f| !locked(source(f, &ctx.launch)))
        .map(|f| (f.key, p.text(f).to_string()))
        .collect();
    values.extend(llm_key.map(|k| (LLM_KEY.key, k)));
    values.extend(model_key.map(|k| (MODEL_KEY.key, k)));
    Ok(Plan {
        cfg,
        llm_key: llm_effective,
        model_key: model_effective,
        values,
    })
}

/// Try `plan` against the real endpoints: the retrieval models as the daemon connects to them at startup, and the
/// generation endpoint's model list (which costs nothing; no completion is requested).
async fn probe(ctx: &Ctx, plan: &Plan) -> Value {
    let eng = &ctx.eng;
    let rerank = plan.cfg.use_reranker && plan.cfg.mem0_rerank;
    let unreachable = match plan.cfg.model_provider {
        ModelProvider::SelfHosted if !mock() => {
            let urls = [
                Some(plan.cfg.embed_url()),
                rerank.then(|| plan.cfg.rerank_url()),
            ];
            let mut down = None;
            for url in urls.into_iter().flatten() {
                if let Err(e) = reachable(url).await {
                    down = Some(e);
                    break;
                }
            }
            down
        }
        _ => None,
    };
    let retrieval = if mock() {
        json!({"ok": true, "mock": true})
    } else if let Some(error) = unreachable {
        json!({"ok": false, "error": error})
    } else {
        match tokio::time::timeout(
            RETRIEVAL_WAIT,
            ModelHub::connect(&plan.cfg, &plan.model_key, rerank),
        )
        .await
        {
            Ok(Ok(hub)) => {
                // The recall floor as the restart settles it, from the configured value: a reranker without a
                // built-in floor fails here, not after the restart.
                let mut cfg = ServiceConfig {
                    mem0_recall_floor: ctx.launch.recall_floor,
                    ..plan.cfg.clone()
                };
                match crate::algorithms::settle_models(&mut cfg, &hub) {
                    Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
                    Ok(()) => json!({
                        "ok": true,
                        "backend": hub.backend,
                        "embedModel": hub.embed_model_id,
                        "rerankModel": hub.rerank_model_id,
                        "dim": hub.dim,
                        // Stored vectors carry the model and dimension; another one re-embeds every memory at the start.
                        "reembed": hub.embed_model_id != eng.models.embed_model_id || hub.dim != eng.models.dim,
                    }),
                }
            }
            Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
            Err(_) => json!({"ok": false, "error": format!(
                "the retrieval models did not answer within {} s", RETRIEVAL_WAIT.as_secs())}),
        }
    };
    let generation = generation(&plan.cfg.llm_base_url, &plan.llm_key, &plan.cfg.llm_model).await;
    let ok = retrieval["ok"] == true && generation["ok"] != false;
    json!({"ok": ok, "retrieval": retrieval, "generation": generation})
}

/// Whether a llama-server answers at `base` at all: one that refuses the connection fails the test at once instead
/// of after the wait for a server that is still loading.
async fn reachable(base: &str) -> Result<(), String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(3));
    if is_loopback(base) {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let url = format!("{}/health", base.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "no llama-server answers at {base} ({e}): start it, or use the one-click install below"
        )),
    }
}

/// The generation endpoint's `GET /models`: `ok` true when it answers with the key, false when it refuses or
/// cannot be reached, null when it cannot tell (no key, or no model list).
async fn generation(base: &str, key: &str, model: &str) -> Value {
    if key.is_empty() {
        return json!({"ok": null, "warning": "no API key is set: memories that need the model to form them will fail"});
    }
    let mut builder = reqwest::Client::builder().timeout(GENERATION_WAIT);
    if is_loopback(base) {
        builder = builder.no_proxy();
    }
    let url = format!("{}/models", base.trim_end_matches('/'));
    let answer = match builder.build() {
        Ok(client) => client.get(&url).bearer_auth(key).send().await,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    let resp = match answer {
        Ok(resp) => resp,
        Err(e) => return json!({"ok": false, "error": format!("cannot reach {url}: {e}")}),
    };
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403) {
        return json!({"ok": false, "error": format!("{url} refused the API key ({status})")});
    }
    if matches!(status.as_u16(), 404 | 405) {
        return json!({"ok": null, "warning": format!(
            "{url} has no model list ({status}), so the key and model cannot be checked before saving")});
    }
    if !status.is_success() {
        let body: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        return json!({"ok": false, "error": format!("{url} returned {status}: {body}")});
    }
    let ids: Vec<String> = resp
        .json::<Value>()
        .await
        .ok()
        .and_then(|v| v["data"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    if !ids.is_empty() && !ids.iter().any(|id| id == model) {
        return json!({"ok": true, "warning": format!("{url} does not list the model {model}")});
    }
    json!({"ok": true})
}

fn respond(res: ApiResult<Value>) -> Response {
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

fn parse(body: Value) -> ApiResult<Proposal> {
    serde_json::from_value(body).map_err(|e| invalid(e.to_string()))
}

async fn test(State(ctx): State<Arc<Ctx>>, Json(body): Json<Value>) -> Response {
    let res = async {
        let plan = plan(&parse(body)?, &ctx)?;
        Ok(probe(&ctx, &plan).await)
    };
    respond(res.await)
}

async fn apply(State(ctx): State<Arc<Ctx>>, Json(body): Json<Value>) -> Response {
    let res = async {
        let Some(env_file) = ctx
            .launch
            .env_file
            .clone()
            .filter(|_| editable(&ctx.launch))
        else {
            return Err(invalid(
                "the daemon was started without --env-file, so the web UI cannot save settings: \
                 install it with `bash scripts/install.sh service --apply`, or edit the settings where it is started",
            ));
        };
        let plan = plan(&parse(body)?, &ctx)?;
        if ctx.restarting.load(Ordering::SeqCst) {
            return Err(invalid(
                "the daemon is already restarting with saved settings",
            ));
        }
        let test = probe(&ctx, &plan).await;
        if test["ok"] != true {
            return Ok(json!({"saved": false, "test": test}));
        }
        if ctx.restarting.swap(true, Ordering::SeqCst) {
            return Err(invalid(
                "the daemon is already restarting with saved settings",
            ));
        }
        let saved = std::fs::read_to_string(&env_file)
            .map_err(anyhow::Error::from)
            .and_then(|text| {
                let values: Vec<(&str, &str)> =
                    plan.values.iter().map(|(k, v)| (*k, v.as_str())).collect();
                settings::save(&env_file, &envfile::with_values(&text, &values))
            });
        if let Err(e) = saved {
            ctx.restarting.store(false, Ordering::SeqCst);
            return Err(ApiError::Internal(format!("{e:#}")));
        }
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            tracing::info!("model settings saved; restarting");
            settings::RESTARTING.store(true, Ordering::SeqCst);
            ctx.eng.shutdown().await;
            let e = ctx.launch.restart();
            tracing::error!("{e:#}");
            std::process::exit(1);
        });
        Ok(json!({"saved": true, "restarting": true, "test": test}))
    };
    respond(res.await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use std::ffi::OsString;
    use tower::ServiceExt;

    const PORT: u16 = 8765;

    async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::post(path)
            .header(header::HOST, format!("127.0.0.1:{PORT}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn proposal(cfg: &ServiceConfig) -> Value {
        json!({"llmBaseUrl": cfg.llm_base_url, "llmModel": cfg.llm_model, "modelProvider": "api"})
    }

    /// The page reads where each setting comes from and never a key; proposals are checked where they enter:
    /// values the env file cannot hold, settings a flag fixes, and a saved key that would go to another host.
    /// (The variables read here are the test process's own: none of these keys is set in it.)
    #[tokio::test]
    async fn settings_show_their_source_and_proposals_are_checked_where_they_enter() {
        let root = tempfile::tempdir().unwrap();
        let cfg = ServiceConfig {
            data_dir: root.path().join("data"),
            ..Default::default()
        };
        let eng = engine(cfg.clone());
        let launch = Launch {
            env_file: None,
            from_file: vec![],
            argv: ["agent-memoryd", "serve", "--llm-model", "qwen-pinned"]
                .map(OsString::from)
                .to_vec(),
            ..Default::default()
        };
        let app = crate::http::router(eng, PORT, None, None, Some(launch));
        let (status, shown) = post(&app, "/admin/model_settings", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(shown["editable"], false);
        assert_eq!(shown["fields"]["llmModel"]["source"], "flag");
        assert_eq!(shown["fields"]["llmModel"]["value"], cfg.llm_model);
        assert_eq!(shown["fields"]["embedUrl"]["source"], "default");
        assert_eq!(shown["fields"]["embedUrl"]["value"], "");
        assert_eq!(shown["fields"]["embedUrl"]["effective"], cfg.embed_url());
        assert_eq!(
            shown["fields"]["llmKey"],
            json!({"set": false, "source": "default"})
        );
        assert!(!shown.to_string().contains("apiKey"), "{shown}");

        let base = proposal(&cfg);
        let with = |patch: Value| {
            let mut p = base.clone();
            for (k, v) in patch.as_object().unwrap() {
                p[k] = v.clone();
            }
            p
        };
        for (bad, why) in [
            (with(json!({"llmModel": "other"})), "--llm-model"),
            (with(json!({"llmBaseUrl": "ftp://x"})), "http(s) URL"),
            (with(json!({"embedUrl": "127.0.0.1:8081"})), "http(s) URL"),
            (
                with(json!({"modelProvider": "local"})),
                "api or self-hosted",
            ),
            (with(json!({"embedModel": " qwen"})), "surrounding spaces"),
            (
                with(json!({"llmKey": "sk-1\nQWEN_BASE_URL=http://evil"})),
                "one line",
            ),
            (with(json!({"modelKey": "\"sk\""})), "quotes"),
            (with(json!({"binary": "/tmp/x"})), "unknown field"),
        ] {
            let (status, body) = post(&app, "/admin/model_settings_test", bad.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert!(
                body["message"].as_str().unwrap().contains(why),
                "{bad}: {body}"
            );
        }
        // Without --env-file nothing is saved.
        let (status, body) = post(&app, "/admin/model_settings_apply", base.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["message"].as_str().unwrap().contains("--env-file"),
            "{body}"
        );
    }

    #[test]
    fn origins_compare_scheme_host_and_port() {
        assert_eq!(
            origin("https://a.example/v1"),
            origin("https://A.example:443/x")
        );
        assert_ne!(
            origin("https://a.example/v1"),
            origin("http://a.example/v1")
        );
        assert_ne!(
            origin("http://127.0.0.1:8081"),
            origin("http://127.0.0.1:8082")
        );
        assert_eq!(origin("not a url"), None);
    }

    #[test]
    fn values_the_env_file_cannot_hold_are_refused() {
        for ok in ["qwen3.8-flash", "http://127.0.0.1:8081/v1", "sk-abc_123"] {
            assert!(check_value("x", ok, 64).is_ok(), "{ok}");
        }
        for bad in [
            "a\nb",
            "a\rb",
            " a",
            "a ",
            "'a'",
            "\"a",
            "a\u{0}",
            &"x".repeat(65),
        ] {
            assert!(check_value("x", bad, 64).is_err(), "{bad:?}");
        }
    }
}
