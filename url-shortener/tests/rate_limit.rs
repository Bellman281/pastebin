//! Integration test: the per-IP rate-limit middleware returns 429 once the
//! bucket is exhausted. Configured with rps=1, burst=1, so the first request
//! passes and the immediate second is rejected. (Oneshot requests carry no
//! peer address, so they share the fallback bucket — exactly what we want here.)

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt; // for `oneshot`

use url_shortener::domain::LinkRepository;
use url_shortener::infrastructure::InMemoryLinkRepository;
use url_shortener::{build_app, Config};

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        max_body_bytes: 16 * 1024,
        database_url: "sqlite::memory:".to_owned(),
        database_max_connections: 1,
        public_base_url: "http://localhost".to_owned(),
        request_timeout_secs: 10,
        max_concurrent_requests: 1024,
        blocked_hosts: Vec::new(),
        rate_limit_rps: 1,
        rate_limit_burst: 1,
    }
}

fn app() -> Router {
    let repo: Arc<dyn LinkRepository> = Arc::new(InMemoryLinkRepository::default());
    build_app(config(), repo)
}

async fn status(app: &Router, uri: &str) -> StatusCode {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn second_request_is_rate_limited() {
    let app = app();
    // First request consumes the single token.
    assert_eq!(status(&app, "/health").await, StatusCode::OK);
    // Immediate second request: bucket empty -> 429.
    assert_eq!(status(&app, "/health").await, StatusCode::TOO_MANY_REQUESTS);
}
