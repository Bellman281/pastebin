//! Top-level error type for the composition root.

use crate::config::ConfigError;

/// Errors that can stop the server from starting or running.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}
