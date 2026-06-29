//! API layer: Axum router, handlers, and DTOs.
//!
//! In PR #1 this exposes only `GET /health`. Paste routes arrive in PR #5.
//! Handlers translate between HTTP and use cases; they hold no business logic.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::AppState;

/// Build the application router from shared, injected state.
///
/// State is `Arc<AppState>`: cloning bumps a refcount rather than copying data.
pub fn router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.max_body_bytes;

    Router::new()
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

/// Liveness probe. A DB-backed readiness check is added in PR #6.
async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
