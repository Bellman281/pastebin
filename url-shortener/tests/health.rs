//! Integration test: the wired app serves `GET /health`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt; // for `oneshot`

use url_shortener::domain::LinkRepository;
use url_shortener::infrastructure::InMemoryLinkRepository;
use url_shortener::{build_app, Config};

fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        max_body_bytes: 16 * 1024,
        database_url: "sqlite::memory:".to_owned(),
        database_max_connections: 1,
        public_base_url: "http://localhost".to_owned(),
        request_timeout_secs: 10,
        max_concurrent_requests: 1024,
    }
}

fn app() -> axum::Router {
    let repo: Arc<dyn LinkRepository> = Arc::new(InMemoryLinkRepository::default());
    build_app(test_config(), repo)
}

#[tokio::test]
async fn health_returns_ok() {
    let response = app()
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn ready_returns_ok_when_store_reachable() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ready");
}
