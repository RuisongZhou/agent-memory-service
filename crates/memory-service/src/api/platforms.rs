//! The web UI's platform page (docs/design/platform-connect.md): `/admin/platforms` and `/admin/connect` run the
//! copy of the installer that `install.sh service` put beside the web UI (`serve --installer`), which alone reads and
//! writes the platforms' configuration. The daemon turns a request into fixed installer arguments and returns the
//! installer's JSON object as it is; its `ok` and `error` say whether the run worked.
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use memory_domain::types::{ApiError, ApiResult};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use super::http::err_response;

/// Longest an installer run may take: `connect --apply` first runs the hooks against a temporary daemon.
const TIMEOUT: Duration = Duration::from_secs(600);

const NOT_INSTALLED: &str = "connecting platforms from the web UI needs the installer copy the service installs: \
     rerun `bash scripts/install.sh service --apply`, or run `bash scripts/install.sh connect` in a terminal";

/// `--installer`, checked once at startup: an executable file.
pub fn check_installer(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::service::is_executable(path),
        "--installer {}: not an executable file",
        path.display()
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Preview,
    Apply,
    Uninstall,
    Rollback,
}

/// `/admin/connect`'s body. `capture` and `recall` choose what `preview` and `apply` install.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectReq {
    host: String,
    action: Action,
    #[serde(default = "yes")]
    capture: bool,
    #[serde(default)]
    recall: bool,
}

fn yes() -> bool {
    true
}

/// The installer arguments of `req`. The host must look like an id (the installer checks which ones exist), so
/// it can never be read as an option.
fn connect_args(req: &ConnectReq) -> ApiResult<Vec<String>> {
    let id = |c: char| c.is_ascii_lowercase() || c == '-';
    if req.host.len() > 32
        || !req.host.starts_with(|c: char| c.is_ascii_lowercase())
        || !req.host.chars().all(id)
    {
        return Err(ApiError::InvalidArgument(format!(
            "host {:?} is not a platform id",
            req.host
        )));
    }
    let mut args = vec![
        "connect".into(),
        "--json".into(),
        "--host".into(),
        req.host.clone(),
    ];
    let flags: &[(bool, &str)] = match req.action {
        Action::Preview => &[(!req.capture, "--no-capture"), (req.recall, "--recall")],
        Action::Apply => &[
            (true, "--apply"),
            (!req.capture, "--no-capture"),
            (req.recall, "--recall"),
        ],
        Action::Uninstall => &[(true, "--uninstall")],
        Action::Rollback => &[(true, "--rollback")],
    };
    args.extend(
        flags
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, flag)| flag.to_string()),
    );
    Ok(args)
}

type Installer = Arc<Option<PathBuf>>;

/// `/admin/platforms` and `/admin/connect`; without `installer` both are a 404 that says how to get it.
pub fn router<S: Clone + Send + Sync + 'static>(installer: Option<PathBuf>) -> Router<S> {
    Router::new()
        .route("/admin/platforms", post(platforms))
        .route("/admin/connect", post(connect))
        .with_state(Arc::new(installer))
}

async fn platforms(State(installer): State<Installer>) -> Response {
    respond(run(&installer, vec!["status".into(), "--json".into()]).await)
}

async fn connect(State(installer): State<Installer>, Json(body): Json<Value>) -> Response {
    let res = async {
        let req: ConnectReq =
            serde_json::from_value(body).map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
        run(&installer, connect_args(&req)?).await
    };
    respond(res.await)
}

fn respond(res: ApiResult<Value>) -> Response {
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

/// Run the installer with `args` and return the JSON object it prints. The run goes on when the request is
/// dropped (a closed page must not cut a write short); it is only waited for up to `TIMEOUT`.
async fn run(installer: &Option<PathBuf>, args: Vec<String>) -> ApiResult<Value> {
    let Some(path) = installer else {
        return Err(ApiError::NotFound(NOT_INSTALLED.into()));
    };
    let mut command = tokio::process::Command::new(path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(TIMEOUT, tokio::spawn(async move { command.output().await }))
        .await
        .map_err(|_| {
            ApiError::Internal(format!(
                "the installer did not finish in {} s",
                TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(format!("cannot run {}: {e}", path.display())))?;
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(value) if value.is_object() => Ok(value),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            let message = format!(
                "the installer printed no result ({}): {reason}",
                output.status
            );
            // Exit 2 is the installer's argument parser refusing the request, such as an unknown host.
            Err(if output.status.code() == Some(2) {
                ApiError::InvalidArgument(message)
            } else {
                ApiError::Internal(message)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use memory_config::ServiceConfig;
    use serde_json::json;
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

    /// The daemon runs the installer with fixed arguments and returns its JSON object, on the daemon's own router;
    /// it refuses a request that could become an option, and without an installer says how to get one.
    #[tokio::test]
    async fn requests_become_fixed_installer_arguments_and_its_json_comes_back() {
        let root = tempfile::tempdir().unwrap();
        let installer = root.path().join("run");
        // Prints its arguments; for host `fail` it refuses as the installer's argument parser does, for `noise` it
        // prints no JSON.
        std::fs::write(
            &installer,
            "#!/bin/sh\ncase \"$*\" in\n\
             *fail*) echo 'install.sh connect: error: argument --host: invalid choice' >&2; exit 2;;\n\
             *noise*) echo oops; echo 'Traceback: boom' >&2; exit 1;;\nesac\n\
             printf '{\"ok\": true, \"args\": \"%s\"}' \"$*\"\n",
        )
        .unwrap();
        assert!(check_installer(&installer).is_err());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
        check_installer(&installer).unwrap();

        let cfg = ServiceConfig {
            data_dir: root.path().join("data"),
            ..Default::default()
        };
        let eng = engine(cfg);
        let app = crate::http::router(eng.clone(), PORT, None, Some(installer));
        let (status, body) = post(&app, "/admin/platforms", json!({})).await;
        assert_eq!(
            (status, &body["args"]),
            (StatusCode::OK, &json!("status --json"))
        );
        for (req, args) in [
            (
                json!({"host": "codex", "action": "preview"}),
                "--host codex",
            ),
            (
                json!({"host": "codex", "action": "apply", "capture": false, "recall": true}),
                "--host codex --apply --no-capture --recall",
            ),
            (
                json!({"host": "workbuddy", "action": "uninstall", "recall": true}),
                "--host workbuddy --uninstall",
            ),
            (
                json!({"host": "claude-code", "action": "rollback"}),
                "--host claude-code --rollback",
            ),
        ] {
            let (status, body) = post(&app, "/admin/connect", req.clone()).await;
            assert_eq!(status, StatusCode::OK, "{req}");
            assert_eq!(body["args"], format!("connect --json {args}"), "{req}");
        }
        for bad in [
            json!({"host": "--apply", "action": "preview"}),
            json!({"host": "Codex", "action": "preview"}),
            json!({"host": "codex", "action": "delete"}),
            json!({"host": "codex", "action": "apply", "binary": "/tmp/x"}),
        ] {
            let (status, body) = post(&app, "/admin/connect", bad.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body["code"], "INVALID_ARGUMENT", "{bad}");
        }
        let (status, body) = post(
            &app,
            "/admin/connect",
            json!({"host": "fail", "action": "preview"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["message"].as_str().unwrap().contains("invalid choice"),
            "{body}"
        );
        let (status, body) = post(
            &app,
            "/admin/connect",
            json!({"host": "noise", "action": "preview"}),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("Traceback: boom"),
            "{body}"
        );
        // The other admin actions keep their route.
        let (status, _) = post(&app, "/admin/stats", json!({})).await;
        assert_eq!(status, StatusCode::OK);

        let app = crate::http::router(eng, PORT, None, None);
        for (path, body) in [
            ("/admin/platforms", json!({})),
            (
                "/admin/connect",
                json!({"host": "codex", "action": "preview"}),
            ),
        ] {
            let (status, body) = post(&app, path, body).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap()
                    .contains("install.sh service --apply"),
                "{path}"
            );
        }
    }
}
