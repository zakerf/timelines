//! Votes at Timeline and Event level (PRD §6, §8).
//!
//! A vote is one `TransactWriteItems`: fixed-window rate-limit counters, the
//! target's `upvotes`/`downvotes` delta, and the VOTE item all commit together.
//! The handler returns the HTMX vote fragment; a vote never re-renders a static
//! page. Counts are baked at render time and refreshed by these fragments.
//!
//! Own-vote state is one Query on `VOTE#<accountId>` with
//! `begins_with(SK, "T#<timelineId>")`, so the client can paint every widget on
//! a timeline in a single round trip.

mod ratelimit;
mod store;

use std::collections::BTreeMap;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use data::model::{Timeline, VoteDirection};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use ratelimit::{hash_ip, trusted_client_ip};
use store::{VoteOutcome, VoteStoreError};

/// A vote body is tiny; anything larger is not a vote.
const MAX_BODY_BYTES: usize = 8 * 1024;

/// Mount the vote routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/timeline/{timeline_id}/vote", post(vote_on_timeline))
        .route(
            "/timeline/{timeline_id}/event/{event_id}/vote",
            post(vote_on_event),
        )
        .route("/timeline/{timeline_id}/votes", get(own_votes))
}

#[derive(Debug, Clone, Deserialize)]
struct DirectionBody {
    #[serde(default, alias = "vote", alias = "value")]
    direction: String,
}

async fn vote_on_timeline(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    match run_vote(
        &state,
        &identity,
        &timeline_id,
        store::VoteTarget::Timeline,
        &headers,
        request,
    )
    .await
    {
        Ok(result) => vote_fragment(&result),
        Err(error) => error.into_response(),
    }
}

async fn vote_on_event(
    State(state): State<AppState>,
    identity: Identity,
    Path((timeline_id, event_id)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    match run_vote(
        &state,
        &identity,
        &timeline_id,
        store::VoteTarget::Event(event_id),
        &headers,
        request,
    )
    .await
    {
        Ok(result) => vote_fragment(&result),
        Err(error) => error.into_response(),
    }
}

struct VoteResult {
    target: String,
    label: &'static str,
    outcome: VoteOutcome,
}

async fn run_vote(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    target: store::VoteTarget,
    headers: &HeaderMap,
    request: Request,
) -> Result<VoteResult, VoteError> {
    let direction = parse_direction(request).await?;
    let ddb = state.dynamodb().map_err(VoteError::Config)?;
    let table = &state.config().table_name;

    let timeline: Timeline = store::load_timeline(ddb, table, timeline_id)
        .await?
        .ok_or(VoteError::NotFound)?;

    // The target's owner is the Timeline owner at both levels (PRD §8).
    if timeline.owner_id == identity.account_id {
        return Err(VoteError::Forbidden);
    }

    let (event, event_id, label) = match &target {
        store::VoteTarget::Timeline => (None, None, "Timeline rating"),
        store::VoteTarget::Event(event_id) => {
            let event = store::load_event(ddb, table, timeline_id, event_id)
                .await?
                .ok_or(VoteError::NotFound)?;
            (Some(event), Some(event_id.clone()), "Event accuracy")
        }
    };

    let old_vote = store::load_vote(
        ddb,
        table,
        &identity.account_id,
        timeline_id,
        event_id.as_deref(),
    )
    .await?;

    let ip_hash = match trusted_client_ip(headers) {
        Some(ip) => Some(hash_ip(&ip, &state.ip_salt().await)),
        None => None,
    };
    let windows = ratelimit::vote_windows(&identity.account_id, ip_hash.as_deref(), Utc::now());
    let now = data::Timestamp::now();

    let outcome = store::cast_vote(
        ddb,
        table,
        store::VoteCommit {
            account_id: &identity.account_id,
            timeline: &timeline,
            event: event.as_ref(),
            old_vote: old_vote.map(|vote| vote.direction),
            new_vote: direction,
            windows: &windows,
            created_at: &now,
        },
    )
    .await?;

    Ok(VoteResult {
        target: event_id.unwrap_or_else(|| timeline.id.clone()),
        label,
        outcome,
    })
}

async fn parse_direction(request: Request) -> Result<Option<VoteDirection>, VoteError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| VoteError::InvalidRequest(format!("could not read the body: {error}")))?;

    let raw = if content_type.contains("application/json") {
        serde_json::from_slice::<DirectionBody>(&bytes)
            .map_err(|error| VoteError::InvalidRequest(format!("invalid JSON: {error}")))?
            .direction
    } else if content_type.contains("application/x-www-form-urlencoded") {
        serde_urlencoded::from_bytes::<DirectionBody>(&bytes)
            .map_err(|error| VoteError::InvalidRequest(format!("invalid form: {error}")))?
            .direction
    } else if bytes.is_empty() {
        String::new()
    } else {
        return Err(VoteError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ));
    };

    parse_direction_value(&raw).ok_or_else(|| {
        VoteError::InvalidRequest(format!(
            "direction must be \"up\", \"down\", or \"none\"; got {raw:?}"
        ))
    })
}

fn parse_direction_value(raw: &str) -> Option<Option<VoteDirection>> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "up" | "+1" => Some(Some(VoteDirection::Up)),
        "down" | "-1" => Some(Some(VoteDirection::Down)),
        "" | "none" | "withdraw" | "clear" => Some(None),
        _ => None,
    }
}

fn vote_fragment(result: &VoteResult) -> Response {
    let target = escape_html(&result.target);
    let up = result.outcome.upvotes;
    let down = result.outcome.downvotes;
    let mine = match result.outcome.vote {
        Some(VoteDirection::Up) => "up",
        Some(VoteDirection::Down) => "down",
        None => "none",
    };

    let html = format!(
        "<div class=\"vote\" data-vote-target=\"{target}\" data-up=\"{up}\" data-down=\"{down}\" \
         data-my-vote=\"{mine}\" aria-label=\"{label}\">\
         <span class=\"vote__count vote__count--up\"><span aria-hidden=\"true\">▲</span>\
         <span class=\"vote__number\">{up}</span></span>\
         <span class=\"vote__count vote__count--down\"><span aria-hidden=\"true\">▼</span>\
         <span class=\"vote__number\">{down}</span></span>\
         <span class=\"vote__label\">{label}</span></div>",
        label = result.label
    );

    response_with(StatusCode::OK, "text/html; charset=utf-8", html)
}

#[derive(Debug, Serialize)]
struct OwnVotesResponse {
    timeline: Option<&'static str>,
    events: BTreeMap<String, &'static str>,
}

async fn own_votes(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
) -> Response {
    match run_own_votes(&state, &identity, &timeline_id).await {
        Ok(response) => response_with(
            StatusCode::OK,
            "application/json; charset=utf-8",
            serde_json::to_string(&response).expect("the own-vote response is serializable"),
        ),
        Err(error) => error.into_response(),
    }
}

async fn run_own_votes(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<OwnVotesResponse, VoteError> {
    let ddb = state.dynamodb().map_err(VoteError::Config)?;
    let votes = store::list_votes(
        ddb,
        &state.config().table_name,
        &identity.account_id,
        timeline_id,
    )
    .await?;

    let mut timeline = None;
    let mut events = BTreeMap::new();
    for vote in votes {
        match vote.event_id {
            Some(event_id) => {
                events.insert(event_id, direction_str(vote.direction));
            }
            None => timeline = Some(direction_str(vote.direction)),
        }
    }

    Ok(OwnVotesResponse { timeline, events })
}

fn direction_str(direction: VoteDirection) -> &'static str {
    match direction {
        VoteDirection::Up => "up",
        VoteDirection::Down => "down",
    }
}

/// Vote-specific refusal. Every branch is one of the contract's shapes:
/// `403` self-vote, `429` + `Retry-After` rate limit, or a coarse client error.
#[derive(Debug)]
enum VoteError {
    InvalidRequest(String),
    Forbidden,
    NotFound,
    RateLimited { retry_after: i64 },
    Conflict,
    Config(ConfigError),
    Internal(String),
}

impl From<VoteStoreError> for VoteError {
    fn from(error: VoteStoreError) -> Self {
        match error {
            VoteStoreError::RateLimited { retry_after } => Self::RateLimited { retry_after },
            VoteStoreError::Conflict => Self::Conflict,
            VoteStoreError::Missing => Self::NotFound,
            VoteStoreError::Internal(message) => Self::Internal(message),
        }
    }
}

impl IntoResponse for VoteError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidRequest(message) => plain(StatusCode::BAD_REQUEST, message),
            Self::Forbidden => plain(
                StatusCode::FORBIDDEN,
                "you cannot vote on your own timeline",
            ),
            Self::NotFound => plain(StatusCode::NOT_FOUND, "vote target not found"),
            Self::RateLimited { retry_after } => {
                let retry_after = retry_after.max(1);
                let mut response = plain(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("vote rate limit exceeded; retry in {retry_after}s"),
                );
                response.headers_mut().insert(
                    RETRY_AFTER,
                    HeaderValue::from_str(&retry_after.to_string())
                        .expect("a positive integer is a valid Retry-After"),
                );
                response
            }
            Self::Conflict => plain(
                StatusCode::CONFLICT,
                "your vote changed concurrently; retry",
            ),
            Self::Config(error) => {
                tracing::error!(%error, "vote refused: configuration incomplete");
                plain(StatusCode::SERVICE_UNAVAILABLE, "voting is unavailable")
            }
            Self::Internal(message) => {
                tracing::error!(%message, "vote failed");
                plain(StatusCode::INTERNAL_SERVER_ERROR, "vote failed")
            }
        }
    }
}

fn plain(status: StatusCode, body: impl Into<String>) -> Response {
    response_with(status, "text/plain; charset=utf-8", body.into())
}

fn response_with(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<axum::body::Body>,
) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("static vote response is valid")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_values_cover_cast_change_and_withdraw() {
        assert_eq!(parse_direction_value("up"), Some(Some(VoteDirection::Up)));
        assert_eq!(parse_direction_value("+1"), Some(Some(VoteDirection::Up)));
        assert_eq!(
            parse_direction_value("DOWN"),
            Some(Some(VoteDirection::Down))
        );
        assert_eq!(parse_direction_value("-1"), Some(Some(VoteDirection::Down)));
        assert_eq!(parse_direction_value("none"), Some(None));
        assert_eq!(parse_direction_value("withdraw"), Some(None));
        assert_eq!(parse_direction_value(""), Some(None));
        assert_eq!(parse_direction_value("sideways"), None);
    }

    #[test]
    fn self_vote_is_forbidden() {
        assert_eq!(
            VoteError::Forbidden.into_response().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn rate_limit_response_sets_retry_after() {
        let response = VoteError::RateLimited { retry_after: 42 }.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "42");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn vote_fragment_carries_both_counts_and_own_vote_state() {
        let response = vote_fragment(&VoteResult {
            target: "t1".to_owned(),
            label: "Timeline rating",
            outcome: VoteOutcome {
                upvotes: 7,
                downvotes: 2,
                vote: Some(VoteDirection::Down),
            },
        });

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8(body.to_vec()).expect("utf-8");
        assert!(html.contains("data-vote-target=\"t1\""), "{html}");
        assert!(html.contains("data-up=\"7\""), "{html}");
        assert!(html.contains("data-down=\"2\""), "{html}");
        assert!(html.contains("data-my-vote=\"down\""), "{html}");
        assert!(html.contains("Timeline rating"), "{html}");
    }

    #[test]
    fn fragment_escapes_the_target_id() {
        let fragment = escape_html("<script>alert('x')</script>");
        assert!(!fragment.contains("<script>"));
        assert!(fragment.contains("&lt;script&gt;"));
    }
}
