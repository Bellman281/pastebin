//! Composition root: load config, build the app, serve with graceful shutdown.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use tokio::net::TcpListener;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use pastebin_service::cache::{Cache, NoOpCache, RedisCache};
use pastebin_service::domain::PasteRepository;
use pastebin_service::infrastructure::SqlitePasteRepository;
use pastebin_service::views::{BatchingViewRecorder, ViewRecorder};
use pastebin_service::{build_app_with_cache_and_views, Config};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "fatal startup error");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    let bind_addr = config.bind_addr;

    let repo: Arc<dyn PasteRepository> = Arc::new(
        SqlitePasteRepository::connect(&config.database_url, config.database_max_connections)
            .await?,
    );

    let cache = build_cache().await;

    // View counts are recorded through a background batcher: each fetch only
    // enqueues its id (a non-blocking channel send), and one task coalesces them
    // into periodic `views = views + n` writes. This keeps the fetch path off
    // the database write lock.
    let (view_recorder, views_task) = BatchingViewRecorder::spawn_default(repo.clone());
    let views: Arc<dyn ViewRecorder> = view_recorder.clone();
    let app = build_app_with_cache_and_views(config, repo, cache, views);

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(%bind_addr, "pastebin-service listening");

    // `into_make_service_with_connect_info` exposes the peer address to the
    // rate-limit middleware so it can key buckets by client IP.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // Graceful shutdown: the server has stopped accepting requests, so flush any
    // buffered view counts before exit (no lost counts), then stop the task.
    view_recorder.flush().await;
    views_task.abort();

    Ok(())
}

/// Build the read-cache from `REDIS_URL`. Empty/unset, or a failed connection,
/// falls back to a no-op cache (the app then reads only from the database).
async fn build_cache() -> Arc<dyn Cache> {
    match std::env::var("REDIS_URL") {
        Ok(url) if !url.is_empty() => match RedisCache::connect(&url).await {
            Ok(cache) => {
                tracing::info!("redis cache enabled");
                Arc::new(cache)
            }
            Err(err) => {
                tracing::warn!(error = %err, "redis unavailable; running without cache");
                Arc::new(NoOpCache)
            }
        },
        _ => Arc::new(NoOpCache),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve on Ctrl-C (and SIGTERM on Unix) so in-flight requests finish and
/// resources drain cleanly — no leaked handles.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
