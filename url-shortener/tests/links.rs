//! End-to-end tests for the link API, driven through the real Axum app over an
//! in-memory repository (no database required).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
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
    }
}

/// One app whose state (the in-memory repo) is shared across cloned routers, so
/// successive requests see each other's writes.
fn app() -> Router {
    let repo: Arc<dyn LinkRepository> = Arc::new(InMemoryLinkRepository::default());
    build_app(config(), repo)
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    // Stash the Location header (if any) as the first "line" for redirect tests.
    if let Some(loc) = location {
        return (status, format!("LOCATION:{loc}").into_bytes());
    }
    (status, bytes)
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn create_with_alias_then_full_lifecycle() {
    let app = app();

    // Create with a custom alias.
    let (status, body) = send(
        &app,
        post_json("/api/links", r#"{"url":"https://example.com","alias":"demo"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = json(&body);
    assert_eq!(created["code"], "demo");
    assert_eq!(created["short_url"], "http://localhost/demo");
    assert_eq!(created["target"], "https://example.com");

    // Metadata: zero hits initially.
    let (status, body) = send(&app, get("/api/links/demo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["hits"], 0);

    // Redirect: 302 with Location, and the hit is counted.
    let (status, body) = send(&app, get("/demo")).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(String::from_utf8(body).unwrap(), "LOCATION:https://example.com");

    let (_, body) = send(&app, get("/api/links/demo")).await;
    assert_eq!(json(&body)["hits"], 1);

    // Delete, then it is gone.
    let (status, _) = send(&app, delete("/api/links/demo")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, get("/api/links/demo")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_without_alias_generates_code() {
    let app = app();
    let (status, body) = send(&app, post_json("/api/links", r#"{"url":"https://example.com"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    let code = json(&body)["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 7);
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
}

#[tokio::test]
async fn invalid_url_is_rejected() {
    let app = app();
    let (status, _) = send(&app, post_json("/api/links", r#"{"url":"ftp://nope"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn duplicate_alias_conflicts() {
    let app = app();
    send(&app, post_json("/api/links", r#"{"url":"https://a.com","alias":"dup"}"#)).await;
    let (status, _) = send(&app, post_json("/api/links", r#"{"url":"https://b.com","alias":"dup"}"#)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn unknown_code_redirect_is_not_found() {
    let app = app();
    let (status, _) = send(&app, get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_with_ttl_sets_expiry_and_still_resolves() {
    let app = app();
    let (status, body) = send(
        &app,
        post_json(
            "/api/links",
            r#"{"url":"https://example.com","alias":"ttl","ttl_seconds":3600}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(json(&body)["expires_at"].is_i64(), "expires_at should be set");

    // Not expired yet → still redirects.
    let (status, _) = send(&app, get("/ttl")).await;
    assert_eq!(status, StatusCode::FOUND);
}

#[tokio::test]
async fn expired_link_redirect_is_not_found() {
    let app = app();
    // ttl_seconds:0 means expires_at == created_at, so it's expired on read.
    let (status, _) = send(
        &app,
        post_json(
            "/api/links",
            r#"{"url":"https://example.com","alias":"exp","ttl_seconds":0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = send(&app, get("/exp")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
