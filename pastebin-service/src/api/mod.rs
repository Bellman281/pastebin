//! API layer: Axum router, handlers, and DTOs.
//!
//! Handlers translate between HTTP and the application's use cases; they hold no
//! business logic. `ServiceError` is mapped to `AppError` here — the only layer
//! that knows about both HTTP and the application.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::application::ServiceError;
use crate::domain::Paste;
use crate::error::AppError;
use crate::AppState;

/// Bridge application errors to HTTP. The only place that knows both vocabularies.
impl From<ServiceError> for AppError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Validation(v) => AppError::Validation(v.to_string()),
            ServiceError::NotFound => AppError::NotFound,
            ServiceError::Conflict => AppError::Conflict("paste id already in use".to_owned()),
            ServiceError::Backend(cause) => AppError::Internal(cause),
        }
    }
}

/// Build the application router from shared, injected state.
pub fn router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.max_body_bytes;

    Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/api/pastes", post(create_paste))
        .route("/api/pastes/:id", get(get_paste).delete(delete_paste))
        .route("/raw/:id", get(raw_paste))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePasteRequest {
    /// The text to store.
    pub content: String,
    /// Optional syntax/language hint (e.g. "rust", "json").
    #[serde(default)]
    pub syntax: Option<String>,
    /// Optional time-to-live in seconds; omit for a paste that never expires.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Burn-after-read: deleted on first fetch. Defaults to false.
    #[serde(default)]
    pub one_shot: bool,
}

#[derive(Debug, Serialize)]
pub struct CreatePasteResponse {
    pub id: String,
    pub url: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub one_shot: bool,
}

impl CreatePasteResponse {
    fn from_paste(paste: &Paste, base_url: &str) -> Self {
        let id = paste.id.as_str();
        Self {
            id: id.to_owned(),
            url: format!("{}/api/pastes/{}", base_url.trim_end_matches('/'), id),
            created_at: paste.created_at,
            expires_at: paste.expires_at,
            one_shot: paste.one_shot,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PasteResponse {
    pub id: String,
    pub content: String,
    pub syntax: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub one_shot: bool,
    pub views: i64,
}

impl PasteResponse {
    fn from_paste(paste: &Paste) -> Self {
        Self {
            id: paste.id.as_str().to_owned(),
            content: paste.content.as_str().to_owned(),
            syntax: paste.syntax.clone(),
            created_at: paste.created_at,
            expires_at: paste.expires_at,
            one_shot: paste.one_shot,
            views: paste.views,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Liveness probe — cheap, no dependencies.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness probe — checks the store; `503` when unreachable.
async fn ready(State(state): State<Arc<AppState>>) -> Response {
    match state.service.ready().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "status": "ready" }))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "unavailable" })),
        )
            .into_response(),
    }
}

/// `POST /api/pastes` — create a paste.
async fn create_paste(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePasteRequest>,
) -> Result<(StatusCode, Json<CreatePasteResponse>), AppError> {
    let paste = state
        .service
        .create(req.content, req.syntax, req.ttl_seconds, req.one_shot)
        .await?;
    let body = CreatePasteResponse::from_paste(&paste, &state.config.public_base_url);
    Ok((StatusCode::CREATED, Json(body)))
}

/// `GET /api/pastes/:id` — fetch metadata + content (counts a view / burns one-shot).
async fn get_paste(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PasteResponse>, AppError> {
    let paste = state.service.fetch(id).await?;
    Ok(Json(PasteResponse::from_paste(&paste)))
}

/// `GET /raw/:id` — raw content as `text/plain` (counts a view / burns one-shot).
async fn raw_paste(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let paste = state.service.fetch(id).await?;
    let mut response = Response::new(Body::from(paste.content.as_str().to_owned()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

/// `DELETE /api/pastes/:id` — remove a paste.
async fn delete_paste(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    state.service.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
