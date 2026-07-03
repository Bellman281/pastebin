//! Pastebin service library crate.
//!
//! The composition root (`main.rs`) builds [`AppState`] and hands it to
//! [`api::router`]. Keeping wiring in the library makes the app testable
//! in-process (see `tests/`).

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod cache;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod metrics;
pub mod rate_limit;
pub mod views;

use std::sync::Arc;

use axum::Router;

pub use config::Config;
pub use error::AppError;

use application::PasteService;
use cache::{Cache, NoOpCache};
use domain::PasteRepository;
use metrics::Metrics;
use rate_limit::RateLimiter;
use views::ViewRecorder;

/// Shared, read-only application state injected into every handler.
///
/// Held behind an `Arc` by the router; `PasteService` only holds an `Arc` to the
/// repository, so the whole state is cheap to share.
pub struct AppState {
    pub config: Config,
    pub service: PasteService,
    pub rate_limiter: RateLimiter,
    /// Lock-free process-wide counters (e.g. pastes served).
    pub metrics: Metrics,
}

/// Build the fully wired Axum application from configuration and a repository.
///
/// The repository is injected as `Arc<dyn PasteRepository>` (Dependency
/// Inversion): production passes the SQLite adapter, tests pass the in-memory
/// double — neither this function nor the handlers change.
pub fn build_app(config: Config, repo: Arc<dyn PasteRepository>) -> Router {
    build_app_with_cache(config, repo, Arc::new(NoOpCache))
}

/// Like [`build_app`] but with an explicit read-cache (e.g. Redis in production).
pub fn build_app_with_cache(
    config: Config,
    repo: Arc<dyn PasteRepository>,
    cache: Arc<dyn Cache>,
) -> Router {
    let rate_limiter = RateLimiter::new(config.rate_limit_rps, config.rate_limit_burst);
    let service = PasteService::with_cache(repo, cache);
    let state = Arc::new(AppState {
        config,
        service,
        rate_limiter,
        metrics: Metrics::default(),
    });
    api::router(state)
}

/// Like [`build_app_with_cache`] but with an explicit view recorder. The
/// composition root passes a [`views::BatchingViewRecorder`] so fetches only
/// enqueue a view and a background task batches the database writes.
pub fn build_app_with_cache_and_views(
    config: Config,
    repo: Arc<dyn PasteRepository>,
    cache: Arc<dyn Cache>,
    views: Arc<dyn ViewRecorder>,
) -> Router {
    let rate_limiter = RateLimiter::new(config.rate_limit_rps, config.rate_limit_burst);
    let service = PasteService::with_cache_and_views(repo, cache, views);
    let state = Arc::new(AppState {
        config,
        service,
        rate_limiter,
        metrics: Metrics::default(),
    });
    api::router(state)
}
