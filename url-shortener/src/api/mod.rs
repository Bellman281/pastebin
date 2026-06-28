//! API layer: Axum router, handlers, and DTOs.
//!
//! Handlers translate between HTTP and the application's use cases; they hold
//! no business logic. Errors flow through `AppError` (one place maps a failure
//! to a status code), and `ServiceError` is mapped to `AppError` here — the
//! only layer that knows about both HTTP and the application.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::application::ServiceError;
use crate::domain::Link;
use crate::error::AppError;
use crate::AppState;

/// Map an application error to its HTTP representation. This is the only bridge
/// between the application and HTTP error vocabularies.
impl From<ServiceError> for AppError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Validation(v) => AppError::Validation(v.to_string()),
            ServiceError::NotFound => AppError::NotFound,
            ServiceError::Conflict => AppError::Conflict("short code already in use".to_owned()),
            ServiceError::Backend(cause) => AppError::Internal(cause),
        }
    }
}

/// Build the application router from shared, injected state.
///
/// State is an `Arc<AppState>`: cloning bumps a refcount, never copies data, so
/// per-request overhead is a pointer clone.
pub fn router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.max_body_bytes;

    Router::new()
        .route("/health", get(health))
        .route("/api/links", post(create_link))
        .route("/api/links/:code", get(get_link).delete(delete_link))
        .route("/:code", get(redirect))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    /// The long URL to shorten.
    pub url: String,
    /// Optional custom alias; if omitted a code is generated.
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateLinkResponse {
    pub code: String,
    pub short_url: String,
    pub target: String,
    pub created_at: i64,
}

impl CreateLinkResponse {
    fn from_link(link: &Link, base_url: &str) -> Self {
        let code = link.code.as_str();
        Self {
            code: code.to_owned(),
            short_url: format!("{}/{}", base_url.trim_end_matches('/'), code),
            target: link.target.as_str().to_owned(),
            created_at: link.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LinkResponse {
    pub code: String,
    pub target: String,
    pub created_at: i64,
    pub hits: i64,
}

impl LinkResponse {
    fn from_link(link: &Link) -> Self {
        Self {
            code: link.code.as_str().to_owned(),
            target: link.target.as_str().to_owned(),
            created_at: link.created_at,
            hits: link.hits,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Liveness probe. A DB-backed readiness check is added in PR #6.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `POST /api/links` — create a short link.
async fn create_link(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLinkRequest>,
) -> Result<(StatusCode, Json<CreateLinkResponse>), AppError> {
    let link = state.service.create(req.url, req.alias).await?;
    let body = CreateLinkResponse::from_link(&link, &state.config.public_base_url);
    Ok((StatusCode::CREATED, Json(body)))
}

/// `GET /:code` — 302 redirect to the original URL (counts the hit).
async fn redirect(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<Response, AppError> {
    let target = state.service.resolve(code).await?;
    let location =
        HeaderValue::from_str(target.as_str()).map_err(|e| AppError::Internal(Box::new(e)))?;

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// `GET /api/links/:code` — fetch link metadata (no hit counted).
async fn get_link(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<Json<LinkResponse>, AppError> {
    let link = state.service.get(code).await?;
    Ok(Json(LinkResponse::from_link(&link)))
}

/// `DELETE /api/links/:code` — remove a link.
async fn delete_link(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<StatusCode, AppError> {
    state.service.delete(code).await?;
    Ok(StatusCode::NO_CONTENT)
}
