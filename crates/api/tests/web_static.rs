//! Static dashboard serving against the real router (ADR-014).
//!
//! Covers runtime-config injection, SPA fallback, asset caching,
//! traversal rejection and the preserved /api error contract. Uses
//! the real database handle because AppState requires one; no rows
//! are touched.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hephaestus_api::{AppState, AuthPolicy, WebSite, router};
use hephaestus_config::WebRuntimeConfig;
use hephaestus_db::Db;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn test_db() -> Db {
    let url = std::env::var("HEPHAESTUS_TEST_DATABASE_URL")
        .unwrap_or_else(|_| panic!("HEPHAESTUS_TEST_DATABASE_URL must be set"));
    let db = Db::connect_with_max(&url, 4)
        .await
        .unwrap_or_else(|e| panic!("connect: {e}"));
    db.migrate()
        .await
        .unwrap_or_else(|e| panic!("migrate: {e}"));
    db
}

async fn app_with_site(site: Option<WebSite>) -> Router {
    let db = test_db().await;
    let mut state = AppState::new(db, AuthPolicy::new(true, &[]), 8 * 1024 * 1024);
    if let Some(site) = site {
        state = state.with_web_site(site);
    }
    router(state)
}

/// A minimal dist fixture shaped like the Vite output.
fn write_dist(dir: &Path) {
    std::fs::create_dir_all(dir.join("assets")).expect("mkdir");
    std::fs::write(
        dir.join("index.html"),
        concat!(
            "<!doctype html><html><head><script>",
            "window.__HEPHAESTUS_WEB_CONFIG__ = window.__HEPHAESTUS_WEB_CONFIG__ ?? {};",
            "</script></head><body><div id=app></div></body></html>"
        ),
    )
    .expect("write index");
    std::fs::write(dir.join("assets/app.js"), "export default 1;").expect("write asset");
    std::fs::write(dir.join("favicon.ico"), "ico").expect("write favicon");
}

fn runtime_config(token: &str) -> WebRuntimeConfig {
    WebRuntimeConfig {
        base_url: String::new(),
        token: token.into(),
        poll_seconds: 7,
    }
}

async fn send_raw(
    app: Router,
    method: &str,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("infallible service");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn index_serves_injected_runtime_config() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dist(dir.path());
    let site =
        WebSite::load(dir.path(), Some(&runtime_config("live-token-1"))).expect("site loads");
    let app = app_with_site(Some(site)).await;

    let (status, headers, body) = send_raw(app.clone(), "GET", "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["content-type"], "text/html; charset=utf-8",
        "entry is html"
    );
    assert!(body.contains("\"live-token-1\""), "token injected: {body}");
    assert!(!body.contains("?? {};"), "placeholder replaced");
    assert_eq!(headers["cache-control"], "no-cache");

    // SPA fallback renders the entry for client-side routes...
    let (status, _, body) = send_raw(
        app.clone(),
        "GET",
        "/tasks/9f2c1c44-0000-0000-0000-000000000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"live-token-1\""), "fallback keeps config");

    // ...but missing hashed assets must not masquerade as HTML.
    let (status, _, body) = send_raw(app, "GET", "/assets/missing.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("<html>"), "asset miss stays plain: {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn assets_carry_hashed_cache_headers_and_mime_types() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dist(dir.path());
    let site = WebSite::load(dir.path(), None).expect("site loads");
    let app = app_with_site(Some(site)).await;

    let (status, headers, body) = send_raw(app, "GET", "/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/javascript; charset=utf-8");
    assert_eq!(
        headers["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body, "export default 1;");
}

#[tokio::test(flavor = "multi_thread")]
async fn api_paths_keep_the_json_error_contract() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dist(dir.path());
    let site = WebSite::load(dir.path(), Some(&runtime_config("x"))).expect("site loads");
    let app = app_with_site(Some(site)).await;

    for path in ["/api/v1/nope", "/api/v1/tasks/not-a-uuid"] {
        let (status, headers, body) = send_raw(app.clone(), "GET", path).await;
        assert_ne!(status, StatusCode::OK, "{path} must not fall back to HTML");
        assert_eq!(headers["content-type"], "application/json", "{path}");
        assert!(body.contains("\"code\""), "error shape preserved: {body}");
    }

    // Probes stay live next to a served dashboard.
    let (status, _, _) = send_raw(app, "GET", "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfigured_server_keeps_json_fallback_everywhere() {
    let app = app_with_site(None).await;
    let (status, headers, body) = send_raw(app, "GET", "/anything").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["content-type"], "application/json");
    assert!(body.contains("not found"), "got: {body}");
}

#[test]
fn load_fails_closed_on_broken_bundles() {
    let dir = tempfile::tempdir().expect("tmp");
    let err = WebSite::load(dir.path(), None).expect_err("missing bundle");
    assert!(err.to_string().contains("dist_dir"), "got: {err}");

    write_dist(dir.path());
    // Placeholder present but runtime config absent is fine...
    WebSite::load(dir.path(), None).expect("plain bundle serves as-is");

    // ...but injecting into a bundle without the placeholder fails.
    std::fs::write(dir.path().join("index.html"), "<html><body>x</body></html>")
        .expect("rewrite index");
    let err = WebSite::load(dir.path(), Some(&runtime_config("t"))).expect_err("placeholder");
    assert!(err.to_string().contains("placeholder"), "got: {err}");
}

#[test]
fn traversal_paths_never_resolve_to_files() {
    // outer/dist is the site root; the secret sits beside it, outside.
    let outer = tempfile::tempdir().expect("tmp");
    let dist = outer.path().join("dist");
    std::fs::create_dir_all(&dist).expect("mkdir dist");
    write_dist(&dist);
    std::fs::write(outer.path().join("secret.txt"), "s3cret").expect("plant secret");

    let site = WebSite::load(&dist, None).expect("site loads");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        for path in [
            "/../secret.txt",
            "/../../secret.txt",
            "/assets/../secret.txt",
        ] {
            let response = site.respond(&axum::http::Method::GET, path).await;
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "{path} escaped the dist root"
            );
        }
    });
}
