//! Integration tests: the server serves the zero-knowledge web client assets.
//! (The browser-side crypto itself is exercised manually / in a browser; these
//! tests cover that the assets are served with the right content types.)

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt; // for `oneshot`

use pastebin_service::domain::PasteRepository;
use pastebin_service::infrastructure::InMemoryPasteRepository;
use pastebin_service::{build_app, Config};

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        max_body_bytes: 1024 * 1024,
        database_url: "sqlite::memory:".to_owned(),
        database_max_connections: 1,
        public_base_url: "http://localhost".to_owned(),
        request_timeout_secs: 10,
        max_concurrent_requests: 1024,
        rate_limit_rps: 0,
        rate_limit_burst: 0,
    }
}

fn app() -> Router {
    let repo: Arc<dyn PasteRepository> = Arc::new(InMemoryPasteRepository::default());
    build_app(config(), repo)
}

async fn get(uri: &str) -> (StatusCode, Option<String>, String) {
    let response = app()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, content_type, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn serves_index_html() {
    let (status, content_type, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().contains("text/html"));
    assert!(body.contains("Zero-Knowledge"));
}

#[tokio::test]
async fn serves_app_js_as_javascript() {
    let (status, content_type, body) = get("/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().contains("javascript"));
    // The client must use authenticated AES-GCM.
    assert!(body.contains("AES-GCM"));
}
