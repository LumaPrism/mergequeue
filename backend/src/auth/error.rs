//! Auth failure modes. Each variant maps to a single HTTP status via
//! `From<AuthError> for poem::Error`; the callback handler intercepts
//! `NotAuthorized` to render the gate's HTML page before that mapping applies.

use poem::Error as PoemError;
use poem::http::StatusCode;
use reqwest::Error as ReqwestError;
use sea_orm::DbErr;

use crate::error::Error as AppError;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("github app not set up")]
    NotConfigured,

    #[error("missing state")]
    MissingState,

    #[error("unknown state")]
    UnknownState,

    #[error("no access token")]
    NoAccessToken,

    #[error("not authorized")]
    NotAuthorized,

    #[error("setup: {0}")]
    Setup(#[from] AppError),

    #[error("database error: {0}")]
    Db(#[from] DbErr),

    #[error("invalid authorize url: {0}")]
    UrlParse(String),

    #[error("github request: {0}")]
    Http(#[from] ReqwestError),
}

impl From<AuthError> for PoemError {
    fn from(e: AuthError) -> Self {
        let status = match &e {
            AuthError::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            AuthError::MissingState | AuthError::UnknownState => StatusCode::BAD_REQUEST,
            AuthError::NoAccessToken | AuthError::Http(_) => StatusCode::BAD_GATEWAY,
            AuthError::NotAuthorized => StatusCode::FORBIDDEN,
            AuthError::Setup(_) | AuthError::Db(_) | AuthError::UrlParse(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        PoemError::from_string(e.to_string(), status)
    }
}

#[cfg(test)]
mod tests {
    use poem::Error as PoemError;
    use poem::http::StatusCode;

    use super::AuthError;

    #[test]
    fn test_auth_error_maps_each_variant_to_its_status() {
        let cases = [
            (AuthError::NotConfigured, StatusCode::SERVICE_UNAVAILABLE),
            (AuthError::MissingState, StatusCode::BAD_REQUEST),
            (AuthError::UnknownState, StatusCode::BAD_REQUEST),
            (AuthError::NoAccessToken, StatusCode::BAD_GATEWAY),
            (AuthError::NotAuthorized, StatusCode::FORBIDDEN),
            (
                AuthError::Db(sea_orm::DbErr::Custom("x".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AuthError::UrlParse("bad".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (err, want) in cases {
            let mapped: PoemError = err.into();
            assert_eq!(mapped.status(), want);
        }
    }
}
