//! Integration test: the wired app serves `GET /health`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt; // for `oneshot`

use pastebin_service::{build_app, Config};

fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        max_body_bytes: 1024 * 1024,
    }
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_app(test_config());

    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
}
