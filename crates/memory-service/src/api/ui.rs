//! Web UI (docs/design/web-ui.md §7.1): the front end's build output, served from `--ui-dir` on the daemon's
//! own origin. The daemon only reads that directory. The front end routes by URL hash, so an unknown path is a
//! 404, never `index.html`.
use axum::http::{header, HeaderValue};
use axum::response::{Redirect, Response};
use axum::routing::get;
use axum::Router;
use memory_domain::types::ApiError;
use std::path::Path;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use super::http::err_response;

/// `--ui-dir`, checked once at startup: a directory holding the front end's `index.html`.
pub fn check_dir(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(dir.is_dir(), "--ui-dir {}: not a directory", dir.display());
    anyhow::ensure!(
        dir.join("index.html").is_file(),
        "--ui-dir {}: no index.html in it",
        dir.display()
    );
    Ok(())
}

/// `/ui`. Without a directory every path is a 404 saying how to install the front end. With one, `/ui`
/// redirects to `/ui/` (so relative asset URLs resolve under it), `/ui/` is `index.html` and `/ui/*` are the
/// directory's files; `ServeDir` refuses paths that leave the directory. Every response must be revalidated,
/// so a browser picks up an upgraded front end on the next load.
pub fn router<S: Clone + Send + Sync + 'static>(dir: Option<&Path>) -> Router<S> {
    let ui = match dir {
        Some(dir) => Router::new()
            .route("/ui", get(|| async { Redirect::temporary("/ui/") }))
            .nest_service("/ui/", ServeDir::new(dir)),
        None => Router::new()
            .route("/ui", get(not_installed))
            .route("/ui/", get(not_installed))
            .route("/ui/{*path}", get(not_installed)),
    };
    ui.layer(SetResponseHeaderLayer::overriding(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    ))
}

async fn not_installed() -> Response {
    err_response(ApiError::NotFound(
        "web UI not installed; start the daemon with --ui-dir PATH".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::engine;
    use axum::body::{to_bytes, Body};
    use axum::http::{HeaderMap, Request, StatusCode};
    use memory_config::ServiceConfig;
    use serde_json::Value;
    use tower::ServiceExt;

    async fn fetch(app: &Router, path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
        (parts.status, parts.headers, body)
    }

    /// The directory is checked at startup and served read-only under `/ui`, on the same router as the API;
    /// without one `/ui` explains how to install the front end. `/health` reports the version the front end
    /// compares with its own.
    #[tokio::test]
    async fn the_ui_directory_is_checked_at_startup_and_served_under_ui() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ui");
        assert!(check_dir(&dir).is_err());
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        assert!(check_dir(&dir).is_err());
        std::fs::write(dir.join("index.html"), "<!doctype html>memory").unwrap();
        std::fs::write(dir.join("assets/app.js"), "export {};").unwrap();
        std::fs::write(root.path().join("secret.txt"), "outside").unwrap();
        check_dir(&dir).unwrap();

        let eng = engine(ServiceConfig::default());
        let without = crate::http::router(eng.clone(), None);
        let (status, _, body) = fetch(&without, "/health").await;
        assert_eq!(status, StatusCode::OK);
        let health: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
        for path in ["/ui", "/ui/", "/ui/assets/app.js"] {
            let (status, headers, body) = fetch(&without, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["code"], "NOT_FOUND");
            assert_eq!(
                body["message"],
                "web UI not installed; start the daemon with --ui-dir PATH"
            );
        }

        let with = crate::http::router(eng, Some(&dir));
        let (status, headers, _) = fetch(&with, "/ui").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(headers[header::LOCATION], "/ui/");
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        let (status, headers, body) = fetch(&with, "/ui/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"<!doctype html>memory");
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        let (status, headers, body) = fetch(&with, "/ui/assets/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"export {};");
        assert!(headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript"));
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        for path in [
            "/ui/missing",
            "/ui/../secret.txt",
            "/ui/%2e%2e/secret.txt",
            "/ui/assets/..%2f..%2fsecret.txt",
        ] {
            let (status, headers, _) = fetch(&with, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        }
    }
}
