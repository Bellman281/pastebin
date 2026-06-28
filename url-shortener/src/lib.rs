//! URL shortener library crate.
//!
//! The composition root (`main.rs`) builds [`AppState`] and hands it to
//! [`api::router`]. Keeping wiring in the library makes the whole app testable
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

use application::LinkService;
use domain::LinkRepository;

/// Shared, read-only application state injected into every handler.
///
/// Held behind an `Arc` by the router; cloning shares ownership rather than
/// duplicating data. `LinkService` itself only holds an `Arc` to the
/// repository, so the whole state is cheap to share.
pub struct AppState {
    pub config: Config,
    pub service: LinkService,
}

/// Build the fully wired Axum application from configuration and a repository.
///
/// The repository is injected as `Arc<dyn LinkRepository>` (Dependency
/// Inversion): production passes the SQLite adapter, tests pass the in-memory
/// double — neither this function nor the handlers change.
pub fn build_app(config: Config, repo: Arc<dyn LinkRepository>) -> Router {
    let service = LinkService::new(repo);
    let state = Arc::new(AppState { config, service });
    api::router(state)
}
