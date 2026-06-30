//! API layer: Axum router, handlers, and DTOs.
//!
//! Handlers translate between HTTP and the application's use cases; they hold
//! no business logic. Errors flow through `AppError` (one place maps a failure
//! to a status code), and `ServiceError` is mapped to `AppError` here — the
//! only layer that knows about both HTTP and the application.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

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
            ServiceError::Blocked => AppError::Validation("target host is not allowed".to_owned()),
            ServiceError::Backend(cause) => AppError::Internal(cause),
        }
    }
}

/// Build the application router from shared, injected state.
///
/// State is an `Arc<AppState>`: cloning bumps a refcount, never copies data, so
/// per-request overhead is a pointer clone.
pub fn router(state: Arc<AppState>) -> Router {
    // Copy the primitives out before `state` is moved into `.with_state`.
    let body_limit = state.config.max_body_bytes;
    let timeout = Duration::from_secs(state.config.request_timeout_secs);
    let max_concurrent = state.config.max_concurrent_requests;

    // Middleware applies outside-in: trace wraps everything (so it logs the
    // final status, including timeouts), CatchPanic turns a handler panic into a
    // 500 instead of dropping the connection, Timeout bounds slow requests
    // (408), ConcurrencyLimit caps in-flight work, and the body limit guards
    // per-request memory.
    let middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .layer(TimeoutLayer::new(timeout))
        .layer(ConcurrencyLimitLayer::new(max_concurrent))
        .layer(DefaultBodyLimit::max(body_limit));

    Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/api/links", post(create_link))
        .route("/api/links/:code", get(get_link).delete(delete_link))
        .route("/:code", get(redirect))
        .layer(middleware)
        // Outermost so abusive clients are rejected before any work is done.
        .layer(axum::middleware::from_fn_with_state(state.clone(), rate_limit))
        .with_state(state)
}

/// Per-IP rate-limit middleware. Rejects with `429` when the client's bucket is
/// empty. Falls back to an unspecified IP when peer info is absent (e.g. when
/// the app is driven directly in tests without `ConnectInfo`).
async fn rate_limit(
    State(state): State<Arc<AppState>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> Response {
    let ip = conn
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if state.rate_limiter.check(ip) {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "rate limit exceeded" })),
        )
            .into_response()
    }
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
    /// Optional time-to-live in seconds; omit for a link that never expires.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CreateLinkResponse {
    pub code: String,
    pub short_url: String,
    pub target: String,
    pub created_at: i64,
    /// Expiry as Unix seconds, or `null` if the link never expires.
    pub expires_at: Option<i64>,
}

impl CreateLinkResponse {
    fn from_link(link: &Link, base_url: &str) -> Self {
        let code = link.code.as_str();
        Self {
            code: code.to_owned(),
            short_url: format!("{}/{}", base_url.trim_end_matches('/'), code),
            target: link.target.as_str().to_owned(),
            created_at: link.created_at,
            expires_at: link.expires_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LinkResponse {
    pub code: String,
    pub target: String,
    pub created_at: i64,
    pub hits: i64,
    /// Expiry as Unix seconds, or `null` if the link never expires.
    pub expires_at: Option<i64>,
}

impl LinkResponse {
    fn from_link(link: &Link) -> Self {
        Self {
            code: link.code.as_str().to_owned(),
            target: link.target.as_str().to_owned(),
            created_at: link.created_at,
            hits: link.hits,
            expires_at: link.expires_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Liveness probe — cheap, no dependencies. Says "the process is up"; it must
/// not depend on the DB, or a DB blip would trigger pod restarts.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness probe — "can this instance serve traffic right now?" Checks the
/// backing store. Returns `503` (not `500`) when the DB is unreachable so a load
/// balancer takes the instance out of rotation instead of treating it as dead.
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

/// `POST /api/links` — create a short link.
async fn create_link(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLinkRequest>,
) -> Result<(StatusCode, Json<CreateLinkResponse>), AppError> {
    let link = state.service.create(req.url, req.alias, req.ttl_seconds).await?;
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
