//! End-to-end tests for the paste API, driven through the real Axum app over an
//! in-memory repository (no database required).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`

use pastebin_service::domain::PasteRepository;
use pastebin_service::infrastructure::InMemoryPasteRepository;
use pastebin_service::{build_app, Config};

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        // Generous body limit so the *content* size check (not the body limit)
        // is what rejects oversized content.
        max_body_bytes: 4 * 1024 * 1024,
        database_url: "sqlite::memory:".to_owned(),
        database_max_connections: 1,
        public_base_url: "http://localhost".to_owned(),
    }
}

fn app() -> Router {
    let repo: Arc<dyn PasteRepository> = Arc::new(InMemoryPasteRepository::default());
    build_app(config(), repo)
}

/// Returns (status, content_type, body bytes).
async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Option<String>, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, content_type, body)
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
async fn create_then_fetch_then_raw_then_delete() {
    let app = app();

    // Create.
    let (status, _, body) = send(
        &app,
        post_json("/api/pastes", r#"{"content":"hello world","syntax":"text"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = json(&body);
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["one_shot"], false);
    assert!(created["url"].as_str().unwrap().ends_with(&format!("/api/pastes/{id}")));

    // Fetch JSON.
    let (status, ct, body) = send(&app, get(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.unwrap().contains("application/json"));
    let fetched = json(&body);
    assert_eq!(fetched["content"], "hello world");
    assert_eq!(fetched["syntax"], "text");

    // Raw.
    let (status, ct, body) = send(&app, get(&format!("/raw/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.unwrap().contains("text/plain"));
    assert_eq!(String::from_utf8(body).unwrap(), "hello world");

    // Delete, then gone.
    let (status, _, _) = send(&app, delete(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&app, get(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn empty_content_is_400() {
    let app = app();
    let (status, _, _) = send(&app, post_json("/api/pastes", r#"{"content":""}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversized_content_is_rejected() {
    let app = app();
    // Just over the 1,000,000-byte content limit (well under the body limit).
    let big = "x".repeat(1_000_001);
    let body = format!(r#"{{"content":"{big}"}}"#);
    let (status, _, _) = send(&app, post_json("/api/pastes", &body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ttl_paste_expires() {
    let app = app();
    // ttl_seconds:0 ⇒ expires_at == created_at ⇒ expired on read.
    let (status, _, body) =
        send(&app, post_json("/api/pastes", r#"{"content":"x","ttl_seconds":0}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = json(&body)["id"].as_str().unwrap().to_owned();

    let (status, _, _) = send(&app, get(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_shot_burns_over_http() {
    let app = app();
    let (status, _, body) = send(
        &app,
        post_json("/api/pastes", r#"{"content":"secret","one_shot":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = json(&body)["id"].as_str().unwrap().to_owned();

    // First fetch succeeds.
    let (status, _, _) = send(&app, get(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    // Second fetch is gone (burned).
    let (status, _, _) = send(&app, get(&format!("/api/pastes/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_id_is_404() {
    let app = app();
    let (status, _, _) = send(&app, get("/api/pastes/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
