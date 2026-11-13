//! Reports and abuse hardening for reportable content (PRD §8).
//!
//! A signed-in Account reports a Timeline, Event, or Suggestion. One REPORT
//! item exists per Account per target, and its existence is the idempotency
//! key: re-reporting the same target is a no-op that consumes no rate-limit
//! slot. The target's `reportCount` moves in the same transaction as the
//! REPORT write; at three distinct accounts the target gains the internal
//! `flagged` state.
//!
//! Reports are anonymous to the owner. The owner read path below exposes counts
//! and flags only — never a reporter id — and nothing here hides or removes
//! reported content.
//!
//! Reporting is limited to 20/day per Account and 100/day per hashed IP (PRD
//! §8). The counters reuse the vote module's fixed-window machinery and are
//! checked inside the report transaction, so a refused report writes nothing.

mod ratelimit;
mod store;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use data::Timestamp;
use data::model::ReportTargetType;

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use crate::http::{hash_ip, json_response, trusted_client_ip};
use ratelimit::report_windows;
use store::{ReportState, ReportStoreError};

/// Mount the report routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/timeline/{timeline_id}/report",
            post(report_timeline).delete(withdraw_timeline),
        )
        .route(
            "/timeline/{timeline_id}/event/{event_id}/report",
            post(report_event).delete(withdraw_event),
        )
        .route(
            "/timeline/{timeline_id}/suggestions/{suggestion_id}/report",
            post(report_suggestion).delete(withdraw_suggestion),
        )
        .route("/timeline/{timeline_id}/reports", get(report_summary))
}

/// One reportable target: the type, the target id, and the Timeline that scopes
/// EVENT and SUGGESTION rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Timeline(String),
    Event {
        timeline_id: String,
        event_id: String,
    },
    Suggestion {
        timeline_id: String,
        suggestion_id: String,
    },
}

impl Target {
    fn timeline(timeline_id: String) -> Self {
        Self::Timeline(timeline_id)
    }

    fn event(timeline_id: String, event_id: String) -> Self {
        Self::Event {
            timeline_id,
            event_id,
        }
    }

    fn suggestion(timeline_id: String, suggestion_id: String) -> Self {
        Self::Suggestion {
            timeline_id,
            suggestion_id,
        }
    }

    fn target_type(&self) -> ReportTargetType {
        match self {
            Self::Timeline(_) => ReportTargetType::Timeline,
            Self::Event { .. } => ReportTargetType::Event,
            Self::Suggestion { .. } => ReportTargetType::Suggestion,
        }
    }

    fn target_id(&self) -> &str {
        match self {
            Self::Timeline(timeline_id) => timeline_id,
            Self::Event { event_id, .. } => event_id,
            Self::Suggestion { suggestion_id, .. } => suggestion_id,
        }
    }

    fn timeline_id(&self) -> &str {
        match self {
            Self::Timeline(timeline_id) | Self::Event { timeline_id, .. } => timeline_id,
            Self::Suggestion { timeline_id, .. } => timeline_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn report_timeline(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match run_report(&state, &identity, Target::timeline(timeline_id), &headers).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn withdraw_timeline(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
) -> Response {
    match run_withdraw(&state, &identity, Target::timeline(timeline_id)).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn report_event(
    State(state): State<AppState>,
    identity: Identity,
    Path((timeline_id, event_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match run_report(
        &state,
        &identity,
        Target::event(timeline_id, event_id),
        &headers,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn withdraw_event(
    State(state): State<AppState>,
    identity: Identity,
    Path((timeline_id, event_id)): Path<(String, String)>,
) -> Response {
    match run_withdraw(&state, &identity, Target::event(timeline_id, event_id)).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn report_suggestion(
    State(state): State<AppState>,
    identity: Identity,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match run_report(
        &state,
        &identity,
        Target::suggestion(timeline_id, suggestion_id),
        &headers,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn withdraw_suggestion(
    State(state): State<AppState>,
    identity: Identity,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
) -> Response {
    match run_withdraw(
        &state,
        &identity,
        Target::suggestion(timeline_id, suggestion_id),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_report(
    state: &AppState,
    identity: &Identity,
    target: Target,
    headers: &HeaderMap,
) -> Result<Response, ReportError> {
    let ddb = state.dynamodb().map_err(ReportError::Config)?;
    let table = &state.config().table_name;

    let row = load_target(ddb, table, &target).await?;

    // Existence is the idempotency key: a second report from the same account
    // returns the current state and consumes no rate-limit slot (PRD §8).
    if store::has_report(
        ddb,
        table,
        target.target_type(),
        target.target_id(),
        &identity.account_id,
    )
    .await
    .map_err(ReportError::Internal)?
    {
        return Ok(state_response(
            StatusCode::OK,
            &target,
            row_state(&row),
            true,
        ));
    }

    let ip_hash = match trusted_client_ip(headers) {
        Some(ip) => Some(hash_ip(&ip, &state.ip_salt().await)),
        None => None,
    };
    let windows = report_windows(&identity.account_id, ip_hash.as_deref(), Utc::now());
    let now = Timestamp::now();

    let result = store::create_report(
        ddb,
        table,
        store::ReportCommit {
            target_type: target.target_type(),
            target_id: target.target_id(),
            account_id: &identity.account_id,
            target: &row,
            windows: &windows,
            created_at: &now,
        },
    )
    .await;

    match result {
        Ok(report_state) => Ok(state_response(
            StatusCode::CREATED,
            &target,
            report_state,
            true,
        )),
        Err(ReportStoreError::AlreadyReported) => {
            let row = load_target(ddb, table, &target).await?;
            Ok(state_response(
                StatusCode::OK,
                &target,
                row_state(&row),
                true,
            ))
        }
        Err(error) => Err(error.into()),
    }
}

async fn run_withdraw(
    state: &AppState,
    identity: &Identity,
    target: Target,
) -> Result<Response, ReportError> {
    let ddb = state.dynamodb().map_err(ReportError::Config)?;
    let table = &state.config().table_name;

    let row = load_target(ddb, table, &target).await?;

    if !store::has_report(
        ddb,
        table,
        target.target_type(),
        target.target_id(),
        &identity.account_id,
    )
    .await
    .map_err(ReportError::Internal)?
    {
        return Ok(state_response(
            StatusCode::OK,
            &target,
            row_state(&row),
            false,
        ));
    }

    let result = store::withdraw_report(
        ddb,
        table,
        target.target_type(),
        target.target_id(),
        &identity.account_id,
        &row,
    )
    .await;

    match result {
        Ok(report_state) => Ok(state_response(StatusCode::OK, &target, report_state, false)),
        Err(ReportStoreError::AlreadyWithdrawn) => {
            let row = load_target(ddb, table, &target).await?;
            Ok(state_response(
                StatusCode::OK,
                &target,
                row_state(&row),
                false,
            ))
        }
        Err(error) => Err(error.into()),
    }
}

async fn load_target(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    target: &Target,
) -> Result<store::TargetRow, ReportError> {
    store::load_target(
        ddb,
        table,
        target.target_type(),
        target.timeline_id(),
        target.target_id(),
    )
    .await
    .map_err(ReportError::Internal)?
    .ok_or(ReportError::NotFound)
}

fn row_state(row: &store::TargetRow) -> ReportState {
    ReportState {
        report_count: row.report_count,
        flagged: row.flagged,
    }
}

/// The owner's report summary: counts and flags only, never a reporter id.
async fn report_summary(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
) -> Response {
    match run_summary(&state, &identity, &timeline_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_summary(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<Response, ReportError> {
    let ddb = state.dynamodb().map_err(ReportError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::load_timeline(ddb, table, timeline_id)
        .await
        .map_err(ReportError::Internal)?
        .ok_or(ReportError::NotFound)?;
    if timeline.owner_id != identity.account_id {
        return Err(ReportError::Forbidden);
    }

    let events = store::load_events(ddb, table, timeline_id)
        .await
        .map_err(ReportError::Internal)?;
    let suggestions = store::load_suggestions(ddb, table, timeline_id)
        .await
        .map_err(ReportError::Internal)?;

    let mut events_json = serde_json::Map::new();
    for event in events {
        events_json.insert(
            event.event_id,
            count_json(event.report_count, event.flagged),
        );
    }
    let mut suggestions_json = serde_json::Map::new();
    for suggestion in suggestions {
        suggestions_json.insert(
            suggestion.suggestion_id,
            count_json(suggestion.report_count, suggestion.flagged),
        );
    }

    Ok(json_response(
        StatusCode::OK,
        &serde_json::json!({
            "timeline": count_json(timeline.report_count, timeline.flagged),
            "events": events_json,
            "suggestions": suggestions_json,
        }),
    ))
}

fn count_json(report_count: i64, flagged: bool) -> serde_json::Value {
    serde_json::json!({"reportCount": report_count, "flagged": flagged})
}

fn state_response(
    status: StatusCode,
    target: &Target,
    state: ReportState,
    reported: bool,
) -> Response {
    json_response(
        status,
        &serde_json::json!({
            "targetType": target.target_type().as_str(),
            "targetId": target.target_id(),
            "reportCount": state.report_count,
            "flagged": state.flagged,
            "reported": reported,
        }),
    )
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Refusals follow the one-of-three contract from PRD §5: `429` +
/// `Retry-After` for a rate limit, a coarse client error otherwise.
#[derive(Debug)]
enum ReportError {
    NotFound,
    Forbidden,
    Conflict,
    RateLimited { retry_after: i64 },
    Config(ConfigError),
    Internal(String),
}

impl From<ReportStoreError> for ReportError {
    fn from(error: ReportStoreError) -> Self {
        match error {
            ReportStoreError::RateLimited { retry_after } => Self::RateLimited { retry_after },
            ReportStoreError::AlreadyReported => Self::Conflict,
            ReportStoreError::AlreadyWithdrawn => Self::Conflict,
            ReportStoreError::Conflict => Self::Conflict,
            ReportStoreError::Internal(message) => Self::Internal(message),
        }
    }
}

impl IntoResponse for ReportError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => json_response(
                StatusCode::NOT_FOUND,
                &serde_json::json!({"error": "not_found", "message": "report target not found"}),
            ),
            Self::Forbidden => json_response(
                StatusCode::FORBIDDEN,
                &serde_json::json!({"error": "forbidden"}),
            ),
            Self::Conflict => json_response(
                StatusCode::CONFLICT,
                &serde_json::json!({
                    "error": "conflict",
                    "message": "the report target changed concurrently; retry",
                }),
            ),
            Self::RateLimited { retry_after } => {
                let retry_after = retry_after.max(1);
                let mut response = json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    &serde_json::json!({"error": "rate_limited", "reason": "report"}),
                );
                response.headers_mut().insert(
                    RETRY_AFTER,
                    HeaderValue::from_str(&retry_after.to_string())
                        .expect("a positive integer is a valid Retry-After"),
                );
                response
            }
            Self::Config(error) => {
                tracing::error!(%error, "report refused: configuration incomplete");
                json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &serde_json::json!({"error": "unavailable"}),
                )
            }
            Self::Internal(message) => {
                tracing::error!(%message, "report failed");
                json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({"error": "internal"}),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CACHE_CONTROL;

    #[test]
    fn each_target_maps_to_its_report_type_and_id() {
        let timeline = Target::timeline("t1".to_owned());
        assert_eq!(timeline.target_type(), ReportTargetType::Timeline);
        assert_eq!(timeline.target_id(), "t1");
        assert_eq!(timeline.timeline_id(), "t1");

        let event = Target::event("t1".to_owned(), "e1".to_owned());
        assert_eq!(event.target_type(), ReportTargetType::Event);
        assert_eq!(event.target_id(), "e1");
        assert_eq!(event.timeline_id(), "t1");

        let suggestion = Target::suggestion("t1".to_owned(), "s1".to_owned());
        assert_eq!(suggestion.target_type(), ReportTargetType::Suggestion);
        assert_eq!(suggestion.target_id(), "s1");
        assert_eq!(suggestion.timeline_id(), "t1");
    }

    #[test]
    fn a_rate_limit_refusal_sets_retry_after() {
        let response = ReportError::RateLimited { retry_after: 42 }.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "42");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[test]
    fn the_summary_shape_carries_counts_and_no_identity() {
        let value = count_json(3, true);
        assert_eq!(value["reportCount"], 3);
        assert_eq!(value["flagged"], true);
        assert_eq!(value.as_object().expect("object").len(), 2);
    }
}
