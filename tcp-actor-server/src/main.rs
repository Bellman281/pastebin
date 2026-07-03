//! Composition root: load config, bind, serve with graceful shutdown.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use tcp_actor_server::{Config, Server, ServerError};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "fatal error");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), ServerError> {
    let config = Config::from_env()?;
    let server = Server::bind(config).await?;
    tracing::info!(addr = %server.local_addr()?, "tcp-actor-server listening");
    server.run(shutdown_signal()).await?;
    tracing::info!("shutdown complete");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve on Ctrl-C (and SIGTERM on Unix) so in-flight connections can drain.
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
