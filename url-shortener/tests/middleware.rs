//! Integration tests for the middleware stack and routing edges: oversized
//! bodies are rejected (413), and unsupported methods on a known path are 405.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use tower::ServiceExt; // for `oneshot`

use url_shortener::domain::LinkRepository;
use url_shortener::infrastructure::InMemoryLinkRepository;
use url_shortener::{build_app, Config};

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        // Small limit so the test can exceed it cheaply.
        max_body_bytes: 1024,
        database_url: "sqlite::memory:".to_owned(),
        database_max_connections: 1,
        public_base_url: "http://localhost".to_owned(),
        request_timeout_secs: 10,
        max_concurrent_requests: 1024,
        blocked_hosts: Vec::new(),
    }
}

fn app() -> Router {
    let repo: Arc<dyn LinkRepository> = Arc::new(InMemoryLinkRepository::default());
    build_app(config(), repo)
}

#[tokio::test]
async fn oversized_body_is_rejected_with_413() {
    // ~4 KiB body against a 1 KiB limit.
    let big = "x".repeat(4096);
    let body = format!(r#"{{"url":"https://example.com/{big}"}}"#);

    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn unsupported_method_on_known_path_is_405() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/links")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
