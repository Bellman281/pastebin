//! Pastebin service library crate.
//!
//! The composition root (`main.rs`) builds [`AppState`] and hands it to
//! [`api::router`]. Keeping wiring in the library makes the app testable
//! in-process (see `tests/`).

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;

use std::sync::Arc;

use axum::Router;

pub use config::Config;
pub use error::AppError;

use application::PasteService;
use domain::PasteRepository;

/// Shared, read-only application state injected into every handler.
///
/// Held behind an `Arc` by the router; `PasteService` only holds an `Arc` to the
/// repository, so the whole state is cheap to share.
pub struct AppState {
    pub config: Config,
    pub service: PasteService,
}

/// Build the fully wired Axum application from configuration and a repository.
///
/// The repository is injected as `Arc<dyn PasteRepository>` (Dependency
/// Inversion): production passes the SQLite adapter, tests pass the in-memory
/// double — neither this function nor the handlers change.
pub fn build_app(config: Config, repo: Arc<dyn PasteRepository>) -> Router {
    let service = PasteService::new(repo);
    let state = Arc::new(AppState { config, service });
    api::router(state)
}
