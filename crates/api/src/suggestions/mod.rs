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
mod queue;
mod request;
mod store;
mod view;

use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use data::Timestamp;
use data::model::{Event, Suggestion, SuggestionKind, SuggestionPayload, SuggestionState};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Identity, OptionalIdentity};
use crate::http::{hash_ip, html_response, json_response, trusted_client_ip};
use crate::publish;

use error::{SuggestionError, retry_after_seconds};
use payload::{
    append_event, apply_edit, checklist_draft, diff_against, field, parse_kind, prefilled,
    proposes_nothing, validate_append, validate_edit,
};
use queue::{QueueEntry, queue_notice_page, queue_page};
use request::{parse_review_body, parse_submit_body};
use view::{
    pending_json, suggest_form_html, suggestion_json, suggestion_notice_html,
    suggestion_submitted_html, suggestions_json,
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

    // The public list names the submitter; an account row that is gone becomes
    // no name at all rather than an opaque id.
    let mut names: Vec<(String, String)> = Vec::new();
    for suggestion in &suggestions {
        if names
            .iter()
            .any(|(account, _)| account == &suggestion.submitted_by)
        {
            continue;
        }
        if let Ok(Some(account)) =
            crate::auth::store::get_account(ddb, table, &suggestion.submitted_by).await
        {
            names.push((suggestion.submitted_by.clone(), account.display_name));
        }
    }

    Ok(json_response(
        StatusCode::OK,
        &pending_json(&suggestions, &names),
    ))
}

/// `GET /api/suggestions/queue` is the JSON contract for clients and the
/// moderation page for browsers: the same URL, negotiated on `Accept`.
async fn owner_queue(
    State(state): State<AppState>,
    identity: Identity,
    headers: HeaderMap,
    Query(query): Query<QueueQuery>,
) -> Response {
    let wants_page = accepts_html(&headers);
    match run_owner_queue(&state, &identity, wants_page, query.done.as_deref()).await {
        Ok(response) => response,
        Err(error) if wants_page => review_notice(&error),
        Err(error) => error.into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct QueueQuery {
    /// The outcome the last review form's redirect carried back.
    done: Option<String>,
}

async fn run_owner_queue(
    state: &AppState,
    identity: &Identity,
    wants_page: bool,
    done: Option<&str>,
) -> Result<Response, SuggestionError> {
    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;
    let suggestions = store::owner_queue(ddb, table, &identity.account_id)
        .await
        .map_err(SuggestionError::Internal)?;

    if !wants_page {
        return Ok(json_response(
            StatusCode::OK,
            &suggestions_json(&suggestions),
        ));
    }

    let mut entries = Vec::with_capacity(suggestions.len());
    for suggestion in suggestions {
        let timeline_title = match store::get_timeline(ddb, table, &suggestion.timeline_id).await {
            Ok(Some(timeline)) => timeline.title,
            Ok(None) => suggestion.timeline_id.clone(),
            Err(error) => return Err(SuggestionError::Internal(error)),
        };
        let submitter =
            match crate::auth::store::get_account(ddb, table, &suggestion.submitted_by).await {
                Ok(Some(account)) => account.display_name,
                Ok(None) => "a reader".to_owned(),
                Err(error) => return Err(SuggestionError::Internal(error.to_string())),
            };
        let target = match &suggestion.payload {
            SuggestionPayload::Edit(_) => match suggestion.target_event_id.as_deref() {
                Some(event_id) => {
                    renderer::dynamo::load_events(ddb, table, &suggestion.timeline_id)
                        .await
                        .map_err(|error| {
                            SuggestionError::Internal(format!("event read failed: {error}"))
                        })?
                        .into_iter()
                        .find(|event| event.event_id == event_id)
                }
                None => None,
            },
            SuggestionPayload::Append(_) => None,
        };
        entries.push(QueueEntry {
            timeline_id: suggestion.timeline_id.clone(),
            timeline_title,
            submitter,
            target,
            suggestion,
        });
    }

    Ok(html_response(
        StatusCode::OK,
        queue_page(&entries, flash_for(done)),
    ))
}

/// `Accept` is how a browser says it wants a document rather than JSON.
fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"))
}

/// The one-line outcome a review form's redirect carries back to the queue.
fn flash_for(done: Option<&str>) -> Option<&'static str> {
    match done {
        Some("approved") => {
            Some("Approved: the change is applied and the affected pages were re-rendered.")
        }
        Some("rejected") => {
            Some("Rejected: the note is recorded and the suggestion left the queue.")
        }
        _ => None,
    }
}

/// A refusal a reviewer sees as a page: the queue's own errors never dump JSON
/// into a browser tab.
fn review_notice(error: &SuggestionError) -> Response {
    let (status, heading, message) = match error {
        SuggestionError::NotFound(what) => (
            StatusCode::NOT_FOUND,
            "Not found",
            format!("That {what} is no longer there."),
        ),
        SuggestionError::Forbidden => (
            StatusCode::FORBIDDEN,
            "Not your suggestion",
            "Only the Timeline's owner can review this suggestion.".to_owned(),
        ),
        SuggestionError::Conflict(message) => (
            StatusCode::CONFLICT,
            "Already reviewed",
            (*message).to_owned(),
        ),
        SuggestionError::Checklist(errors) => (
            StatusCode::BAD_REQUEST,
            "Could not apply that change",
            errors
                .iter()
                .map(|error| error.message.clone())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        SuggestionError::RateLimited { .. } => (
            StatusCode::TOO_MANY_REQUESTS,
            "Slow down",
            "Try that again in a moment.".to_owned(),
        ),
        other => {
            tracing::warn!(?other, "review refused");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "That action could not be completed. Try again.".to_owned(),
            )
        }
    };
    html_response(status, queue_notice_page(heading, &message))
}

async fn approve(
    State(state): State<AppState>,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
    identity: Identity,
    request: Request,
) -> Response {
    let from_form = is_form_post(&request);
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
        Ok(_) if from_form => review_redirect("approved"),
        Ok(response) => response,
        Err(error) if from_form => review_notice(&error),
        Err(error) => error.into_response(),
    }
}

async fn reject(
    State(state): State<AppState>,
    Path((timeline_id, suggestion_id)): Path<(String, String)>,
    identity: Identity,
    request: Request,
) -> Response {
    let from_form = is_form_post(&request);
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
        Ok(_) if from_form => review_redirect("rejected"),
        Ok(response) => response,
        Err(error) if from_form => review_notice(&error),
        Err(error) => error.into_response(),
    }
}

/// A browser review ends back on the queue, with its outcome in the URL.
fn review_redirect(outcome: &str) -> Response {
    Redirect::to(&format!("/api/suggestions/queue?done={outcome}")).into_response()
}

/// A urlencoded POST is the queue page's own form, not an API client.
fn is_form_post(request: &Request) -> bool {
    request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/x-www-form-urlencoded"))
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
