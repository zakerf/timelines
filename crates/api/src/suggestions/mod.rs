//! Suggestions end-to-end (PRD §6 access patterns 5/6/9/10, PRD §8 limits).
//!
//! Three surfaces share one moderation flow:
//!
//! - a signed-in Account submits an `APPEND` or `EDIT` Suggestion; the write is
//!   a single transaction with the rate-limit counters and the per-Timeline
//!   pending cap so a refusal never leaves a partial row;
//! - anyone can read a Timeline's pending Suggestions, and an owner reads every
//!   pending Suggestion across all their Timelines through the sparse GSI1
//!   queue;
//! - the owner approves or rejects with a note. Approval flips the state in the
//!   same transaction that applies the content change: APPEND puts the Event
//!   and bumps `eventCount`, EDIT rewrites the Event and stamps
//!   `lastEditedBy`/`lastEditSuggestionId`. Edit approval re-runs the publish
//!   checklist before committing, so the back door stays closed.
//!
//! This module owns routing and the request flow; [`request`] parses bodies,
//! [`payload`] validates and applies content, [`view`] renders responses, and
//! [`error`] maps refusals to HTTP.

mod error;
mod payload;
mod request;
mod store;
mod view;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use data::Timestamp;
use data::model::{Event, Suggestion, SuggestionKind, SuggestionPayload, SuggestionState};

use crate::AppState;
use crate::auth::{Identity, OptionalIdentity};
use crate::http::{hash_ip, html_response, json_response, trusted_client_ip};
use crate::publish;

use error::{SuggestionError, retry_after_seconds};
use payload::{
    append_event, apply_edit, checklist_draft, diff_against, field, parse_kind, prefilled,
    proposes_nothing, validate_append, validate_edit,
};
use request::{parse_review_body, parse_submit_body};
use view::{
    suggest_form_html, suggestion_json, suggestion_notice_html, suggestion_submitted_html,
    suggestions_json,
};

pub use store::{SubmitLimit, SubmitLimits};

/// Mount the suggestion routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/timeline/{timeline_id}/suggestions",
            get(list_pending).post(submit),
        )
        .route(
            "/timeline/{timeline_id}/event/{event_id}/suggest",
            get(suggest_form),
        )
        .route(
            "/timeline/{timeline_id}/suggestions/{suggestion_id}/approve",
            post(approve),
        )
        .route(
            "/timeline/{timeline_id}/suggestions/{suggestion_id}/reject",
            post(reject),
        )
        .route("/suggestions/queue", get(owner_queue))
}

async fn submit(
    State(state): State<AppState>,
    Path(timeline_id): Path<String>,
    identity: Identity,
    headers: HeaderMap,
    request: Request,
) -> Response {
    match run_submit(&state, &timeline_id, &identity, &headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// The "Suggest an edit" form page (PRD §3). Served to any signed-in Account;
/// anonymous visitors go through login. The form starts at the Event's current
/// values, so a correction needs no retyping.
async fn suggest_form(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path((timeline_id, event_id)): Path<(String, String)>,
) -> Response {
    if identity.is_none() {
        return crate::auth::login_redirect(&format!(
            "/api/timeline/{timeline_id}/event/{event_id}/suggest"
        ));
    }
    match load_event(&state, &timeline_id, &event_id).await {
        Ok(event) => {
            let html = suggest_form_html(&timeline_id, &event, &prefilled(&event), &[]);
            html_response(StatusCode::OK, html)
        }
        Err(response) => response,
    }
}

/// The Event a suggestion targets, or the page explaining why it is not there.
async fn load_event(
    state: &AppState,
    timeline_id: &str,
    event_id: &str,
) -> Result<Event, Response> {
    let ddb = match state.dynamodb() {
        Ok(ddb) => ddb,
        Err(error) => {
            tracing::error!(%error, "suggest form refused: configuration incomplete");
            return Err(html_response(
                StatusCode::SERVICE_UNAVAILABLE,
                suggestion_notice_html(
                    timeline_id,
                    "Suggestions are unavailable",
                    "This page cannot be shown right now. Try again in a moment.",
                ),
            ));
        }
    };
    let events = renderer::dynamo::load_events(ddb, &state.config().table_name, timeline_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "suggest form refused: event read failed");
            html_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                suggestion_notice_html(
                    timeline_id,
                    "Suggestions are unavailable",
                    "This page cannot be shown right now. Try again in a moment.",
                ),
            )
        })?;
    events
        .into_iter()
        .find(|event| event.event_id == event_id)
        .ok_or_else(|| {
            html_response(
                StatusCode::NOT_FOUND,
                suggestion_notice_html(
                    timeline_id,
                    "Event not found",
                    "This Event is no longer on the Timeline, so there is nothing to correct.",
                ),
            )
        })
}

async fn run_submit(
    state: &AppState,
    timeline_id: &str,
    identity: &Identity,
    headers: &HeaderMap,
    request: Request,
) -> Result<Response, SuggestionError> {
    let body = parse_submit_body(request).await?;
    let from_form = body.from_form;
    let request = body.request;
    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(SuggestionError::Internal)?
        .ok_or(SuggestionError::NotFound("timeline"))?;

    let now = Timestamp::now();
    let kind = parse_kind(&request.kind)?;

    let (payload, target_event_id, label, confirmed) = match kind {
        SuggestionKind::Append => {
            let append = validate_append(&request.payload)?;
            let label = append.title.clone();
            (SuggestionPayload::Append(append), None, label, None)
        }
        SuggestionKind::Edit => {
            let target = request
                .target_event_id
                .as_deref()
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .ok_or_else(|| {
                    SuggestionError::Checklist(vec![field(
                        "targetEventId",
                        "an edit names the event it changes",
                    )])
                })?;
            let events = renderer::dynamo::load_events(ddb, table, timeline_id)
                .await
                .map_err(|error| {
                    SuggestionError::Internal(format!("event read failed: {error}"))
                })?;
            let current = events
                .iter()
                .find(|event| event.event_id == target)
                .ok_or(SuggestionError::NotFound("event"))?
                .clone();
            // The browser form is prefilled, so only what the reader actually
            // changed is proposed. A JSON client sends exactly what it means.
            let proposed = if from_form {
                diff_against(&current, &request.payload)
            } else {
                request.payload.clone()
            };
            let validated = if from_form && proposes_nothing(&proposed) {
                Err(SuggestionError::Checklist(vec![field(
                    "payload",
                    "nothing changed: edit at least one field, or cancel",
                )]))
            } else {
                validate_edit(&proposed)
            };
            let edit = match validated {
                Ok(edit) => edit,
                Err(error) if from_form => {
                    return Ok(refused_form(timeline_id, &current, &request.payload, error));
                }
                Err(error) => return Err(error),
            };
            let label = edit
                .title
                .clone()
                .unwrap_or_else(|| format!("edit-{target}"));
            (
                SuggestionPayload::Edit(edit),
                Some(target.to_owned()),
                label,
                Some(current),
            )
        }
    };

    let suggestion = Suggestion {
        suggestion_id: publish::generated_id(&label),
        timeline_id: timeline_id.to_owned(),
        owner_id: Some(timeline.owner_id.clone()),
        state: SuggestionState::Pending,
        kind,
        target_event_id,
        payload,
        submitted_by: identity.account_id.clone(),
        submitted_at: now.clone(),
        reviewed_by: None,
        reviewed_at: None,
        review_note: None,
        report_count: 0,
        flagged: false,
    };

    let ip_hash = match trusted_client_ip(headers) {
        Some(ip) => {
            let salt = state.ip_salt().await;
            Some(hash_ip(&ip, &salt))
        }
        None => None,
    };

    store::submit(
        ddb,
        table,
        &suggestion,
        &SubmitLimits::default(),
        &now,
        ip_hash.as_deref(),
    )
    .await
    .map_err(|failure| match failure {
        store::SubmitFailure::RateLimited(limit) => SuggestionError::RateLimited {
            limit,
            retry_after: retry_after_seconds(&now, limit),
        },
        store::SubmitFailure::Store(message) => SuggestionError::Internal(message),
    })?;

    // A human gets the pending-review page; a JSON client keeps the contract.
    if from_form && let Some(event) = confirmed {
        return Ok(html_response(
            StatusCode::CREATED,
            suggestion_submitted_html(timeline_id, &event),
        ));
    }

    Ok(json_response(
        StatusCode::CREATED,
        &suggestion_json(&suggestion),
    ))
}

/// A refused browser submission: the form again, carrying what was typed and
/// the message for the field that failed.
fn refused_form(
    timeline_id: &str,
    event: &Event,
    submitted: &request::PayloadRequest,
    error: SuggestionError,
) -> Response {
    let errors = match error {
        SuggestionError::Checklist(errors) => errors,
        other => {
            tracing::warn!(?other, "suggestion refused without field errors");
            Vec::new()
        }
    };
    let html = suggest_form_html(timeline_id, event, submitted, &errors);
    html_response(StatusCode::BAD_REQUEST, html)
}

async fn list_pending(State(state): State<AppState>, Path(timeline_id): Path<String>) -> Response {
    match run_list_pending(&state, &timeline_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_list_pending(
    state: &AppState,
    timeline_id: &str,
) -> Result<Response, SuggestionError> {
    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;

    if store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(SuggestionError::Internal)?
        .is_none()
    {
        return Err(SuggestionError::NotFound("timeline"));
    }

    let suggestions = store::list_pending(ddb, table, timeline_id)
        .await
        .map_err(SuggestionError::Internal)?;
    Ok(json_response(
        StatusCode::OK,
        &suggestions_json(&suggestions),
    ))
}

async fn owner_queue(State(state): State<AppState>, identity: Identity) -> Response {
    match run_owner_queue(&state, &identity).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_owner_queue(
    state: &AppState,
    identity: &Identity,
) -> Result<Response, SuggestionError> {
    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;
    let suggestions = store::owner_queue(ddb, table, &identity.account_id)
        .await
        .map_err(SuggestionError::Internal)?;
    Ok(json_response(
        StatusCode::OK,
        &suggestions_json(&suggestions),
    ))
}

async fn approve(
    State(state): State<AppState>,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
    identity: Identity,
    request: Request,
) -> Response {
    match run_review(
        &state,
        &timeline_id,
        &suggestion_id,
        &identity,
        request,
        Decision::Approve,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn reject(
    State(state): State<AppState>,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
    identity: Identity,
    request: Request,
) -> Response {
    match run_review(
        &state,
        &timeline_id,
        &suggestion_id,
        &identity,
        request,
        Decision::Reject,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Approve,
    Reject,
}

async fn run_review(
    state: &AppState,
    timeline_id: &str,
    suggestion_id: &str,
    identity: &Identity,
    request: Request,
    decision: Decision,
) -> Result<Response, SuggestionError> {
    let body = parse_review_body(request).await?;
    let note = body
        .note
        .map(|note| note.trim().to_owned())
        .filter(|note| !note.is_empty());
    if decision == Decision::Reject && note.is_none() {
        return Err(SuggestionError::InvalidRequest(
            "a rejection needs a note".to_owned(),
        ));
    }

    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(SuggestionError::Internal)?
        .ok_or(SuggestionError::NotFound("timeline"))?;
    if timeline.owner_id != identity.account_id {
        return Err(SuggestionError::Forbidden);
    }

    let suggestion = store::get_suggestion(ddb, table, timeline_id, suggestion_id)
        .await
        .map_err(SuggestionError::Internal)?
        .ok_or(SuggestionError::NotFound("suggestion"))?;
    if suggestion.state != SuggestionState::Pending {
        return Err(SuggestionError::Conflict(
            "this suggestion has already been reviewed",
        ));
    }

    let now = Timestamp::now();
    let review = store::Review {
        reviewer: &identity.account_id,
        note: note.as_deref(),
        now: &now,
    };

    let mut reviewed = suggestion.clone();
    reviewed.reviewed_by = Some(identity.account_id.clone());
    reviewed.reviewed_at = Some(now.clone());
    reviewed.review_note = note.clone();

    match decision {
        Decision::Reject => {
            reviewed.state = SuggestionState::Rejected;
            store::review(
                ddb,
                table,
                &suggestion,
                review,
                store::ReviewOutcome::Reject,
            )
            .await
            .map_err(review_failure)?;
        }
        Decision::Approve => {
            reviewed.state = SuggestionState::Approved;
            match &suggestion.payload {
                SuggestionPayload::Append(append) => {
                    let event = append_event(&timeline, append, &now)?;
                    store::review(
                        ddb,
                        table,
                        &suggestion,
                        review,
                        store::ReviewOutcome::ApproveAppend(&event),
                    )
                    .await
                    .map_err(review_failure)?;
                }
                SuggestionPayload::Edit(edit) => {
                    let events = renderer::dynamo::load_events(ddb, table, timeline_id)
                        .await
                        .map_err(|error| {
                            SuggestionError::Internal(format!("event read failed: {error}"))
                        })?;
                    let original = events
                        .iter()
                        .find(|event| Some(&event.event_id) == suggestion.target_event_id.as_ref())
                        .cloned()
                        .ok_or(SuggestionError::NotFound("event"))?;
                    let updated = apply_edit(
                        &original,
                        edit,
                        &suggestion.submitted_by,
                        &suggestion.suggestion_id,
                    );

                    let mut after = events.clone();
                    if let Some(slot) = after
                        .iter_mut()
                        .find(|event| event.event_id == original.event_id)
                    {
                        *slot = updated.clone();
                    }

                    publish::validate(&checklist_draft(&timeline, &after))
                        .map_err(SuggestionError::Checklist)?;

                    let change = store::EditChange { original, updated };
                    store::review(
                        ddb,
                        table,
                        &suggestion,
                        review,
                        store::ReviewOutcome::ApproveEdit(&change),
                    )
                    .await
                    .map_err(review_failure)?;
                }
            }

            publish::rerender_timeline(state, &timeline)
                .await
                .map_err(SuggestionError::Internal)?;
        }
    }

    Ok(json_response(StatusCode::OK, &suggestion_json(&reviewed)))
}

fn review_failure(failure: store::ReviewFailure) -> SuggestionError {
    match failure {
        store::ReviewFailure::NotPending => {
            SuggestionError::Conflict("this suggestion has already been reviewed")
        }
        store::ReviewFailure::Conflict(message) => SuggestionError::Conflict(message),
        store::ReviewFailure::Store(message) => SuggestionError::Internal(message),
    }
}
