//! API layer: Axum router, handlers, and DTOs.
//!
//! Handlers translate between HTTP and the application's use cases; they hold no
//! business logic. `ServiceError` is mapped to `AppError` here — the only layer
//! that knows about both HTTP and the application.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::application::ServiceError;
use crate::domain::Paste;
use crate::error::AppError;
use crate::AppState;

/// The zero-knowledge web client, embedded at compile time. The server serves
/// these assets but never sees plaintext or keys — all crypto happens in the
/// browser and the key travels only in the URL fragment.
const INDEX_HTML: &str = include_str!("../../static/index.html");
const APP_JS: &str = include_str!("../../static/app.js");
/// Vendored QR-code generator (qrcode-generator, MIT, © Kazuhiko Arase). Served
/// locally so QR codes are generated in-browser — the share link (with its key
/// fragment) never goes to a third party.
const QRCODE_JS: &str = include_str!("../../static/vendor/qrcode.js");

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
    // Copy primitives out before `state` is moved into `.with_state`.
    let body_limit = state.config.max_body_bytes;
    let timeout = Duration::from_secs(state.config.request_timeout_secs);
    let max_concurrent = state.config.max_concurrent_requests;

    // Outside-in: trace wraps everything, CatchPanic turns a handler panic into
    // a 500, Timeout bounds slow requests (408), ConcurrencyLimit caps in-flight
    // work, and the body limit guards per-request memory.
    let middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .layer(TimeoutLayer::new(timeout))
        .layer(ConcurrencyLimitLayer::new(max_concurrent))
        .layer(DefaultBodyLimit::max(body_limit));

    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/vendor/qrcode.js", get(qrcode_js))
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/pastes", post(create_paste))
        .route("/api/pastes/:id", get(get_paste).delete(delete_paste))
        .route("/raw/:id", get(raw_paste))
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

/// Serve the zero-knowledge web client (the SPA reads `#<id>.<key>` itself).
async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Serve the client script with a JavaScript content type.
async fn app_js() -> Response {
    javascript(APP_JS)
}

/// Serve the vendored QR-code generator.
async fn qrcode_js() -> Response {
    javascript(QRCODE_JS)
}

/// Build a `200 application/javascript` response for a static script.
fn javascript(body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response
}

/// Liveness probe — cheap, no dependencies.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `GET /metrics` — process-wide, lock-free counters (read with an atomic load).
async fn metrics(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pastes_served": state.metrics.pastes_served() }))
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
    state.metrics.record_served(); // lock-free atomic bump
    Ok(Json(PasteResponse::from_paste(&paste)))
}

/// `GET /raw/:id` — raw content as `text/plain` (counts a view / burns one-shot).
async fn raw_paste(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let paste = state.service.fetch(id).await?;
    state.metrics.record_served(); // lock-free atomic bump
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
