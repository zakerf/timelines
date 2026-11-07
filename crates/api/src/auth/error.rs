//! Failures from the auth seam, rendered as plain-text HTTP responses.

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::config::ConfigError;

/// An auth endpoint failure. The status stays coarse so the browser flow needs
/// no JSON error envelope; details are logged, never reflected.
#[derive(Debug)]
pub struct AuthError {
    status: StatusCode,
    message: String,
}

impl AuthError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AuthError {}

impl From<ConfigError> for AuthError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::MissingSsmClient | ConfigError::MissingDynamoClient => {
                Self::unavailable(error.to_string())
            }
            other => Self::internal(other.to_string()),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        tracing::warn!(status = %self.status, message = %self.message, "auth request failed");
        (self.status, self.message).into_response()
    }
}
