//! URL shortener library crate.
//!
//! The composition root (`main.rs`) builds [`AppState`] and hands it to
//! [`api::router`]. Keeping wiring in the library makes the whole app testable
//! in-process (see `tests/`).

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod cache;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod rate_limit;

use std::sync::Arc;

use axum::Router;

pub use config::Config;
pub use error::AppError;

use application::LinkService;
use cache::{Cache, NoOpCache};
use domain::LinkRepository;
use rate_limit::RateLimiter;

/// Shared, read-only application state injected into every handler.
///
/// Held behind an `Arc` by the router; cloning shares ownership rather than
/// duplicating data. `LinkService` itself only holds an `Arc` to the
/// repository, so the whole state is cheap to share.
pub struct AppState {
    pub config: Config,
    pub service: LinkService,
    pub rate_limiter: RateLimiter,
}

/// Build the fully wired Axum application from configuration and a repository.
///
/// The repository is injected as `Arc<dyn LinkRepository>` (Dependency
/// Inversion): production passes the SQLite adapter, tests pass the in-memory
/// double — neither this function nor the handlers change.
pub fn build_app(config: Config, repo: Arc<dyn LinkRepository>) -> Router {
    build_app_with_cache(config, repo, Arc::new(NoOpCache))
}

/// Like [`build_app`] but with an explicit read-cache (e.g. Redis in production).
pub fn build_app_with_cache(
    config: Config,
    repo: Arc<dyn LinkRepository>,
    cache: Arc<dyn Cache>,
) -> Router {
    let rate_limiter = RateLimiter::new(config.rate_limit_rps, config.rate_limit_burst);
    let service = LinkService::with_cache(repo, config.blocked_hosts.clone(), cache);
    let state = Arc::new(AppState { config, service, rate_limiter });
    api::router(state)
}
