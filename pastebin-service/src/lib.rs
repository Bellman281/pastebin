//! Pastebin service library crate.
//!
//! The composition root (`main.rs`) builds [`AppState`] and hands it to
//! [`api::router`]. Keeping wiring in the library makes the app testable
//! in-process (see `tests/`).

#![forbid(unsafe_code)]

// Layers are added in their own PRs: `application` (#3), `infrastructure` (#4).
pub mod api;
pub mod config;
pub mod domain;
pub mod error;

use std::sync::Arc;

use axum::Router;

pub use config::Config;
pub use error::AppError;

/// Shared, read-only application state injected into every handler.
pub struct AppState {
    pub config: Config,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

/// Build the fully wired Axum application from configuration.
pub fn build_app(config: Config) -> Router {
    let state = Arc::new(AppState::new(config));
    api::router(state)
}
