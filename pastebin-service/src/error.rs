//! Single application error type and its HTTP representation.
//!
//! Handlers return `Result<T, AppError>`; this is the only place that decides
//! how a failure becomes an HTTP status + JSON body. No `unwrap()`/`panic!` on
//! the request path.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// A thread-safe boxed standard error carrying the cause of an internal failure
/// without leaking its detail to clients.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// All error cases the API can surface to a client.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    Conflict(String),

    #[error("internal error")]
    Internal(#[source] BoxedError),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if let AppError::Internal(err) = &self {
            tracing::error!(error = %err, "internal error");
        }
        let body = Json(json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_maps_to_its_status() {
        assert_eq!(AppError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            AppError::Validation("bad".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Conflict("dup".into()).status(),
            StatusCode::CONFLICT
        );
        let internal = AppError::Internal("boom".into());
        assert_eq!(internal.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn into_response_sets_status() {
        let resp = AppError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
