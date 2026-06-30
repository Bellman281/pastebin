//! Composition root: load config, build the app, serve with graceful shutdown.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use tokio::net::TcpListener;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use url_shortener::domain::LinkRepository;
use url_shortener::infrastructure::SqliteLinkRepository;
use url_shortener::{build_app, Config};

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

    let repo: Arc<dyn LinkRepository> = Arc::new(
        SqliteLinkRepository::connect(&config.database_url, config.database_max_connections).await?,
    );

    let app = build_app(config, repo);

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(%bind_addr, "url-shortener listening");

    // `into_make_service_with_connect_info` exposes the peer address to the
    // rate-limit middleware so it can key buckets by client IP.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve on Ctrl-C (and SIGTERM on Unix) so in-flight requests can finish and
/// resources (DB pool in later PRs) drain cleanly — no leaked handles.
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
