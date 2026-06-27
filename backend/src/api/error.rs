//! The API's error type. Handlers build `ApiError` values and return
//! `poem::Result`; `From<ApiError> for poem::Error` maps each variant to the
//! exact status code and message the dashboard already expects, so the HTTP
//! surface is unchanged. Internal errors flow in via `#[from]` so `?`
//! propagates cleanly.

use poem::Error;
use poem::http::StatusCode;

use crate::error::Error as AppError;

/// Every failure the REST API can return, tagged with the HTTP status it maps to.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// An internal application error bubbling up from the engine/runtime stack — 500.
    #[error(transparent)]
    App(#[from] AppError),

    /// A database failure, surfaced as a 500 with the driver's message.
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),

    /// No valid session cookie — 401.
    #[error("unauthorized")]
    Unauthorized,

    /// Authenticated but lacking write access to the resource — 403.
    #[error("forbidden")]
    Forbidden,

    /// A malformed path or query parameter — 400.
    #[error("{0}")]
    BadRequest(String),

    /// A referenced resource does not exist — 404.
    #[error("{0}")]
    NotFound(String),

    /// A well-formed request that fails a domain rule — 422.
    #[error("{0}")]
    Validation(String),

    /// A request that conflicts with current state — 409.
    #[error("{0}")]
    Conflict(String),

    /// An upstream GitHub failure — 502.
    #[error("{0}")]
    BadGateway(String),

    /// An internal failure carrying a client-safe message — 500.
    #[error("{0}")]
    Internal(String),
}

impl From<ApiError> for Error {
    fn from(err: ApiError) -> Self {
        let status = match &err {
            ApiError::App(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Error::from_string(err.to_string(), status)
    }
}
