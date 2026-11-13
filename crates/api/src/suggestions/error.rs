//! The one-of-three refusal contract from PRD §5 for the suggestion surfaces.

use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use data::Timestamp;

use super::store::SubmitLimit;
use crate::config::ConfigError;
use crate::http::json_response;
use crate::publish::FieldError;

/// Refusals follow the one-of-three contract from PRD §5.
#[derive(Debug)]
pub(super) enum SuggestionError {
    InvalidRequest(String),
    Checklist(Vec<FieldError>),
    NotFound(&'static str),
    Forbidden,
    Conflict(&'static str),
    RateLimited {
        limit: SubmitLimit,
        retry_after: u64,
    },
    Config(ConfigError),
    Internal(String),
}

impl IntoResponse for SuggestionError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidRequest(message) => json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "invalid_request", "message": message}),
            ),
            Self::Checklist(errors) => json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "checklist_failed", "errors": errors}),
            ),
            Self::NotFound(what) => json_response(
                StatusCode::NOT_FOUND,
                &serde_json::json!({"error": "not_found", "message": format!("{what} not found")}),
            ),
            Self::Forbidden => json_response(
                StatusCode::FORBIDDEN,
                &serde_json::json!({"error": "forbidden"}),
            ),
            Self::Conflict(message) => json_response(
                StatusCode::CONFLICT,
                &serde_json::json!({"error": "conflict", "message": message}),
            ),
            Self::RateLimited { limit, retry_after } => {
                let mut response = json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    &serde_json::json!({
                        "error": "rate_limited",
                        "reason": limit.as_str(),
                    }),
                );
                if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
                    response.headers_mut().insert(RETRY_AFTER, value);
                }
                response
            }
            Self::Config(error) => {
                tracing::error!(%error, "suggestion refused: configuration incomplete");
                json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &serde_json::json!({"error": "unavailable"}),
                )
            }
            Self::Internal(message) => {
                tracing::error!(%message, "suggestion failed");
                json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({"error": "internal"}),
                )
            }
        }
    }
}

/// Seconds left in the fixed window, for `Retry-After` (PRD §8).
pub(super) fn retry_after_seconds(now: &Timestamp, limit: SubmitLimit) -> u64 {
    let value = now.as_str().as_bytes();
    let number = |range: std::ops::Range<usize>| -> u64 {
        std::str::from_utf8(&value[range])
            .ok()
            .and_then(|part| part.parse().ok())
            .unwrap_or(0)
    };
    let hour = number(11..13);
    let minute = number(14..16);
    let second = number(17..19);

    match limit {
        SubmitLimit::AccountHourly | SubmitLimit::IpHourly => 3600 - (minute * 60 + second),
        SubmitLimit::AccountDaily => 86_400 - (hour * 3600 + minute * 60 + second),
        // Not a window: the cap frees up when the owner reviews. Report the
        // conservative next-hour hint rather than no hint at all.
        SubmitLimit::PendingCap => 3600,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_points_at_the_end_of_the_fixed_window() {
        let now = Timestamp::parse("2026-11-08T12:34:56.789Z").expect("test timestamp");
        assert_eq!(
            retry_after_seconds(&now, SubmitLimit::AccountHourly),
            3600 - (34 * 60 + 56)
        );
        assert_eq!(
            retry_after_seconds(&now, SubmitLimit::AccountDaily),
            86_400 - (12 * 3600 + 34 * 60 + 56)
        );
    }
}
