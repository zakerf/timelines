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

mod store;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use data::Timestamp;
use data::model::{
    AppendPayload, EditPayload, Emphasis, Event, Suggestion, SuggestionKind, SuggestionPayload,
    SuggestionState, Timeline,
};
use serde::Deserialize;

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use crate::publish::{self, EventDraft, FieldError, PublishDraft, is_exact_date, is_http_source};

pub use store::{SubmitLimit, SubmitLimits};

/// Largest submission payload accepted.
const MAX_BODY_BYTES: usize = 128 * 1024;
/// Trusted edge header carrying the viewer address (PRD §8). Never a
/// client-authored forwarding header.
const VIEWER_ADDRESS_HEADER: &str = "cloudfront-viewer-address";

/// Mount the suggestion routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/timeline/{timeline_id}/suggestions",
            get(list_pending).post(submit),
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

// ---------------------------------------------------------------------------
// Request and response shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitRequest {
    kind: String,
    #[serde(default)]
    target_event_id: Option<String>,
    #[serde(default)]
    payload: PayloadRequest,
}

/// Every field is optional because `EDIT` carries only what the author wants to
/// change and `APPEND` supplies the rest. Validation picks the required set.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayloadRequest {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    emphasis: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    note: Option<String>,
}

/// The owner's pending queue (GSI1) or a Timeline's public pending list.
fn suggestions_json(suggestions: &[Suggestion]) -> serde_json::Value {
    serde_json::Value::Array(suggestions.iter().map(suggestion_json).collect())
}

fn suggestion_json(suggestion: &Suggestion) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert(
        "id".to_owned(),
        serde_json::Value::String(suggestion.suggestion_id.clone()),
    );
    value.insert(
        "timelineId".to_owned(),
        serde_json::Value::String(suggestion.timeline_id.clone()),
    );
    value.insert(
        "state".to_owned(),
        serde_json::Value::String(suggestion.state.as_str().to_owned()),
    );
    value.insert(
        "kind".to_owned(),
        serde_json::Value::String(suggestion.kind.as_str().to_owned()),
    );
    if let Some(target) = &suggestion.target_event_id {
        value.insert(
            "targetEventId".to_owned(),
            serde_json::Value::String(target.clone()),
        );
    }
    value.insert("payload".to_owned(), payload_json(&suggestion.payload));
    value.insert(
        "submittedBy".to_owned(),
        serde_json::Value::String(suggestion.submitted_by.clone()),
    );
    value.insert(
        "submittedAt".to_owned(),
        serde_json::Value::String(suggestion.submitted_at.as_str().to_owned()),
    );
    if let Some(reviewer) = &suggestion.reviewed_by {
        value.insert(
            "reviewedBy".to_owned(),
            serde_json::Value::String(reviewer.clone()),
        );
    }
    if let Some(reviewed_at) = &suggestion.reviewed_at {
        value.insert(
            "reviewedAt".to_owned(),
            serde_json::Value::String(reviewed_at.as_str().to_owned()),
        );
    }
    if let Some(note) = &suggestion.review_note {
        value.insert(
            "reviewNote".to_owned(),
            serde_json::Value::String(note.clone()),
        );
    }
    serde_json::Value::Object(value)
}

fn payload_json(payload: &SuggestionPayload) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    match payload {
        SuggestionPayload::Append(append) => {
            value.insert(
                "date".to_owned(),
                serde_json::Value::String(append.date.clone()),
            );
            value.insert(
                "title".to_owned(),
                serde_json::Value::String(append.title.clone()),
            );
            value.insert(
                "description".to_owned(),
                serde_json::Value::String(append.description.clone()),
            );
            value.insert(
                "sources".to_owned(),
                serde_json::Value::Array(
                    append
                        .sources
                        .iter()
                        .map(|source| serde_json::Value::String(source.clone()))
                        .collect(),
                ),
            );
            value.insert(
                "emphasis".to_owned(),
                serde_json::Value::String(append.emphasis.as_str().to_owned()),
            );
        }
        SuggestionPayload::Edit(edit) => {
            if let Some(date) = &edit.date {
                value.insert("date".to_owned(), serde_json::Value::String(date.clone()));
            }
            if let Some(title) = &edit.title {
                value.insert("title".to_owned(), serde_json::Value::String(title.clone()));
            }
            if let Some(description) = &edit.description {
                value.insert(
                    "description".to_owned(),
                    serde_json::Value::String(description.clone()),
                );
            }
            if let Some(sources) = &edit.sources {
                value.insert(
                    "sources".to_owned(),
                    serde_json::Value::Array(
                        sources
                            .iter()
                            .map(|source| serde_json::Value::String(source.clone()))
                            .collect(),
                    ),
                );
            }
            if let Some(emphasis) = edit.emphasis {
                value.insert(
                    "emphasis".to_owned(),
                    serde_json::Value::String(emphasis.as_str().to_owned()),
                );
            }
        }
    }
    serde_json::Value::Object(value)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

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

async fn run_submit(
    state: &AppState,
    timeline_id: &str,
    identity: &Identity,
    headers: &HeaderMap,
    request: Request,
) -> Result<Response, SuggestionError> {
    let request = parse_submit_body(request).await?;
    let ddb = state.dynamodb().map_err(SuggestionError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(SuggestionError::Internal)?
        .ok_or(SuggestionError::NotFound("timeline"))?;

    let now = Timestamp::now();
    let kind = parse_kind(&request.kind)?;

    let (payload, target_event_id, label) = match kind {
        SuggestionKind::Append => {
            let append = validate_append(&request.payload)?;
            let label = append.title.clone();
            (SuggestionPayload::Append(append), None, label)
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
            let edit = validate_edit(&request.payload)?;
            let events = renderer::dynamo::load_events(ddb, table, timeline_id)
                .await
                .map_err(|error| {
                    SuggestionError::Internal(format!("event read failed: {error}"))
                })?;
            if !events.iter().any(|event| event.event_id == target) {
                return Err(SuggestionError::NotFound("event"));
            }
            let label = edit
                .title
                .clone()
                .unwrap_or_else(|| format!("edit-{target}"));
            (
                SuggestionPayload::Edit(edit),
                Some(target.to_owned()),
                label,
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

    let ip_hash = match client_ip(headers) {
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

    Ok(json_response(
        StatusCode::CREATED,
        &suggestion_json(&suggestion),
    ))
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

// ---------------------------------------------------------------------------
// Payload validation and application
// ---------------------------------------------------------------------------

fn parse_kind(value: &str) -> Result<SuggestionKind, SuggestionError> {
    match value.trim().to_ascii_uppercase().as_str() {
        "APPEND" => Ok(SuggestionKind::Append),
        "EDIT" => Ok(SuggestionKind::Edit),
        other => Err(SuggestionError::InvalidRequest(format!(
            "kind must be APPEND or EDIT, got {other:?}"
        ))),
    }
}

fn validate_append(payload: &PayloadRequest) -> Result<AppendPayload, SuggestionError> {
    let date = payload.date.clone().unwrap_or_default();
    let title = payload.title.clone().unwrap_or_default();
    let description = payload.description.clone().unwrap_or_default();
    let sources = clean_sources(payload.sources.as_deref().unwrap_or_default());
    let mut errors = Vec::new();

    if !is_exact_date(&date) {
        errors.push(field(
            "payload.date",
            "exact date as YYYY-MM-DD is required",
        ));
    }
    if title.trim().is_empty() {
        errors.push(field("payload.title", "event title is required"));
    }
    if !sources.iter().any(|source| is_http_source(source)) {
        errors.push(field(
            "payload.sources",
            "at least one http(s) source URL is required",
        ));
    }
    let emphasis = parse_emphasis(payload.emphasis.as_deref(), "payload.emphasis", &mut errors);

    if !errors.is_empty() {
        return Err(SuggestionError::Checklist(errors));
    }

    Ok(AppendPayload {
        date: date.trim().to_owned(),
        title: title.trim().to_owned(),
        description: description.trim().to_owned(),
        sources,
        emphasis,
    })
}

fn validate_edit(payload: &PayloadRequest) -> Result<EditPayload, SuggestionError> {
    let mut errors = Vec::new();

    if payload.title.is_none()
        && payload.description.is_none()
        && payload.date.is_none()
        && payload.sources.is_none()
        && payload.emphasis.is_none()
    {
        errors.push(field("payload", "at least one proposed field is required"));
    }
    if let Some(date) = &payload.date
        && !is_exact_date(date)
    {
        errors.push(field(
            "payload.date",
            "exact date as YYYY-MM-DD is required",
        ));
    }
    if let Some(title) = &payload.title
        && title.trim().is_empty()
    {
        errors.push(field("payload.title", "event title cannot be blank"));
    }
    if let Some(sources) = &payload.sources
        && !sources.iter().any(|source| is_http_source(source))
    {
        errors.push(field(
            "payload.sources",
            "at least one http(s) source URL is required",
        ));
    }
    let emphasis = if payload.emphasis.is_some() {
        Some(parse_emphasis(
            payload.emphasis.as_deref(),
            "payload.emphasis",
            &mut errors,
        ))
    } else {
        None
    };

    if !errors.is_empty() {
        return Err(SuggestionError::Checklist(errors));
    }

    Ok(EditPayload {
        title: payload.title.as_ref().map(|title| title.trim().to_owned()),
        description: payload.description.clone(),
        date: payload.date.as_ref().map(|date| date.trim().to_owned()),
        sources: payload.sources.as_deref().map(clean_sources),
        emphasis,
    })
}

fn parse_emphasis(
    value: Option<&str>,
    field_name: &'static str,
    errors: &mut Vec<FieldError>,
) -> Emphasis {
    match value.map(str::trim) {
        None | Some("") => Emphasis::Standard,
        Some(value) => match value.parse() {
            Ok(emphasis) => emphasis,
            Err(_) => {
                errors.push(field(field_name, "emphasis must be standard or featured"));
                Emphasis::Standard
            }
        },
    }
}

fn clean_sources(sources: &[String]) -> Vec<String> {
    sources
        .iter()
        .map(|source| source.trim().to_owned())
        .filter(|source| !source.is_empty())
        .collect()
}

fn field(name: &str, message: &str) -> FieldError {
    FieldError {
        field: name.to_owned(),
        message: message.to_owned(),
    }
}

/// Apply an accepted EDIT to the stored Event. `lastEditedBy` attributes the
/// change to the Suggestion's author; the approving owner is recorded on the
/// Suggestion itself.
fn apply_edit(event: &Event, edit: &EditPayload, editor: &str, suggestion_id: &str) -> Event {
    let mut updated = event.clone();
    if let Some(date) = &edit.date {
        updated.date = date.clone();
        updated.timestamp =
            Timestamp::from_date(&updated.date).expect("validate_edit checked the exact date");
    }
    if let Some(title) = &edit.title {
        updated.title = title.clone();
    }
    if let Some(description) = &edit.description {
        updated.description = description.trim().to_owned();
    }
    if let Some(sources) = &edit.sources {
        updated.sources = sources.clone();
    }
    if let Some(emphasis) = edit.emphasis {
        updated.emphasis = emphasis;
    }
    updated.last_edited_by = Some(editor.to_owned());
    updated.last_edit_suggestion_id = Some(suggestion_id.to_owned());
    updated
}

/// An approved APPEND becomes a normal EVENT row, built by the publish
/// checklist's own constructor so the shapes cannot drift.
fn append_event(
    timeline: &Timeline,
    append: &AppendPayload,
    now: &Timestamp,
) -> Result<Event, SuggestionError> {
    let draft = PublishDraft {
        title: timeline.title.clone(),
        description: timeline.description.clone(),
        collection_id: timeline.collection_id.clone(),
        events: vec![EventDraft {
            date: append.date.clone(),
            title: append.title.clone(),
            description: append.description.clone(),
            sources: append.sources.clone(),
            emphasis: Some(append.emphasis.as_str().to_owned()),
        }],
    };
    let event_id = publish::generated_id(&append.title);
    let built = publish::build(
        &draft,
        &timeline.owner_id,
        timeline.id.clone(),
        vec![event_id],
        now.clone(),
    )
    .map_err(SuggestionError::Checklist)?;
    built
        .events
        .into_iter()
        .next()
        .ok_or_else(|| SuggestionError::Internal("checklist built no event".to_owned()))
}

/// Re-run the publish checklist against the Timeline as the EDIT would leave
/// it. This is the gate PRD §8 calls "the back door is closed too".
fn checklist_draft(timeline: &Timeline, events: &[Event]) -> PublishDraft {
    PublishDraft {
        title: timeline.title.clone(),
        description: timeline.description.clone(),
        collection_id: timeline.collection_id.clone(),
        events: events
            .iter()
            .map(|event| EventDraft {
                date: event.date.clone(),
                title: event.title.clone(),
                description: event.description.clone(),
                sources: event.sources.clone(),
                emphasis: Some(event.emphasis.as_str().to_owned()),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Abuse-prevention helpers
// ---------------------------------------------------------------------------

/// The trusted viewer address, without its port. Only
/// `CloudFront-Viewer-Address` is read: a client-authored forwarding header
/// must never pick the rate-limit partition.
fn client_ip(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(VIEWER_ADDRESS_HEADER)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(bare_ip(value))
}

fn bare_ip(value: &str) -> String {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_owned();
    }
    // A bare IPv6 address has several colons; only an IPv4:port pair has one.
    if value.matches(':').count() == 1 {
        return value.split(':').next().unwrap_or(value).to_owned();
    }
    value.to_owned()
}

fn hash_ip(ip: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(ip.as_bytes());
    hasher.update(salt.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Seconds left in the fixed window, for `Retry-After` (PRD §8).
fn retry_after_seconds(now: &Timestamp, limit: SubmitLimit) -> u64 {
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

// ---------------------------------------------------------------------------
// Bodies and responses
// ---------------------------------------------------------------------------

async fn parse_submit_body(request: Request) -> Result<SubmitRequest, SuggestionError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.contains("application/json") {
        return Err(SuggestionError::InvalidRequest(
            "send application/json".to_owned(),
        ));
    }
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            SuggestionError::InvalidRequest(format!("could not read the body: {error}"))
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| SuggestionError::InvalidRequest(format!("invalid JSON: {error}")))
}

async fn parse_review_body(request: Request) -> Result<ReviewRequest, SuggestionError> {
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            SuggestionError::InvalidRequest(format!("could not read the body: {error}"))
        })?;
    if bytes.is_empty() {
        return Ok(ReviewRequest::default());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| SuggestionError::InvalidRequest(format!("invalid JSON: {error}")))
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(value.to_string()))
        .expect("static JSON response is valid")
}

/// Refusals follow the one-of-three contract from PRD §5.
#[derive(Debug)]
enum SuggestionError {
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

#[cfg(test)]
mod tests {
    use super::*;
    use data::item::DynamoItem;
    use data::model::SuggestionState;

    const NOW: &str = "2026-11-08T12:34:56.789Z";
    const SOURCE: &str = "https://example.org/source";

    fn now() -> Timestamp {
        Timestamp::parse(NOW).expect("test timestamp")
    }

    fn append_payload() -> PayloadRequest {
        PayloadRequest {
            date: Some("2026-02-04".to_owned()),
            title: Some("Candidate filing deadline".to_owned()),
            description: Some("Petitions were due.".to_owned()),
            sources: Some(vec![SOURCE.to_owned()]),
            emphasis: None,
        }
    }

    fn sample_event() -> Event {
        Event {
            timeline_id: "t1".to_owned(),
            event_id: "e1".to_owned(),
            timestamp: Timestamp::from_date("2026-02-04").unwrap(),
            date: "2026-02-04".to_owned(),
            title: "Old title".to_owned(),
            description: "Old description".to_owned(),
            sources: vec![SOURCE.to_owned()],
            emphasis: Emphasis::Standard,
            upvotes: 3,
            downvotes: 1,
            report_count: 0,
            flagged: false,
            created_at: now(),
            created_by: "owner".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        }
    }

    fn sample_timeline() -> Timeline {
        Timeline {
            id: "t1".to_owned(),
            title: "A Timeline".to_owned(),
            description: "Deck".to_owned(),
            owner_id: "owner".to_owned(),
            collection_id: Some("c1".to_owned()),
            event_count: 1,
            upvotes: 0,
            downvotes: 0,
            created_at: now(),
            updated_at: now(),
            report_count: 0,
            flagged: false,
            cover_image: None,
        }
    }

    fn fields(error: SuggestionError) -> Vec<String> {
        match error {
            SuggestionError::Checklist(errors) => {
                errors.into_iter().map(|error| error.field).collect()
            }
            other => panic!("expected a checklist refusal, got {other:?}"),
        }
    }

    #[test]
    fn append_keeps_the_full_proposed_event() {
        let append = validate_append(&append_payload()).expect("valid append");
        assert_eq!(append.date, "2026-02-04");
        assert_eq!(append.title, "Candidate filing deadline");
        assert_eq!(append.sources, [SOURCE]);
        assert_eq!(append.emphasis, Emphasis::Standard);
    }

    #[test]
    fn append_requires_a_date_title_and_http_source() {
        let payload = PayloadRequest {
            date: Some("February 4".to_owned()),
            title: Some("  ".to_owned()),
            sources: Some(vec!["not a url".to_owned()]),
            ..append_payload()
        };
        assert_eq!(
            fields(validate_append(&payload).expect_err("invalid append")),
            ["payload.date", "payload.title", "payload.sources"]
        );
    }

    #[test]
    fn append_rejects_an_unknown_emphasis() {
        let payload = PayloadRequest {
            emphasis: Some("shouting".to_owned()),
            ..append_payload()
        };
        assert_eq!(
            fields(validate_append(&payload).expect_err("invalid emphasis")),
            ["payload.emphasis"]
        );
    }

    #[test]
    fn edit_keeps_only_the_proposed_fields() {
        let payload = PayloadRequest {
            title: Some("  Better title ".to_owned()),
            ..PayloadRequest::default()
        };
        let edit = validate_edit(&payload).expect("valid edit");
        assert_eq!(edit.title.as_deref(), Some("Better title"));
        assert_eq!(edit.description, None);
        assert_eq!(edit.date, None);
        assert_eq!(edit.sources, None);
        assert_eq!(edit.emphasis, None);
    }

    #[test]
    fn edit_requires_at_least_one_field() {
        assert_eq!(
            fields(validate_edit(&PayloadRequest::default()).expect_err("empty edit")),
            ["payload"]
        );
    }

    #[test]
    fn edit_validates_only_the_fields_it_proposes() {
        let payload = PayloadRequest {
            date: Some("2026-02-30".to_owned()),
            title: Some(String::new()),
            sources: Some(vec!["javascript:alert(1)".to_owned()]),
            ..PayloadRequest::default()
        };
        assert_eq!(
            fields(validate_edit(&payload).expect_err("invalid edit")),
            ["payload.date", "payload.title", "payload.sources"]
        );
    }

    #[test]
    fn an_accepted_edit_applies_in_place_and_stamps_attribution() {
        let event = sample_event();
        let edit = EditPayload {
            title: Some("New title".to_owned()),
            description: Some(" New description ".to_owned()),
            ..EditPayload::default()
        };
        let updated = apply_edit(&event, &edit, "author-2", "s1");

        assert_eq!(updated.title, "New title");
        assert_eq!(updated.description, "New description");
        assert_eq!(updated.timestamp, event.timestamp);
        assert_eq!(updated.upvotes, event.upvotes);
        assert_eq!(updated.created_by, event.created_by);
        assert_eq!(updated.last_edited_by.as_deref(), Some("author-2"));
        assert_eq!(updated.last_edit_suggestion_id.as_deref(), Some("s1"));
    }

    #[test]
    fn a_date_edit_moves_the_event_key() {
        let event = sample_event();
        let edit = EditPayload {
            date: Some("2026-05-05".to_owned()),
            ..EditPayload::default()
        };
        let updated = apply_edit(&event, &edit, "author-2", "s1");
        assert_eq!(updated.date, "2026-05-05");
        assert_eq!(
            updated.timestamp.as_str(),
            "2026-05-05T00:00:00.000Z",
            "date-only events get midnight UTC"
        );
        assert_ne!(updated.sk(), event.sk());
    }

    #[test]
    fn the_edit_checklist_rejects_a_timeline_with_a_bad_event() {
        let timeline = sample_timeline();
        let mut bad = sample_event();
        bad.sources = vec!["not a url".to_owned()];

        let errors = publish::validate(&checklist_draft(&timeline, &[bad]))
            .expect_err("the checklist catches the pre-existing bad event");
        assert_eq!(errors[0].field, "events[0].sources");
    }

    #[test]
    fn the_edit_checklist_accepts_a_valid_timeline() {
        let timeline = sample_timeline();
        assert_eq!(
            publish::validate(&checklist_draft(&timeline, &[sample_event()])),
            Ok(())
        );
    }

    #[test]
    fn suggestion_json_has_the_camel_case_contract() {
        let suggestion = Suggestion {
            suggestion_id: "s1".to_owned(),
            timeline_id: "t1".to_owned(),
            owner_id: Some("owner".to_owned()),
            state: SuggestionState::Pending,
            kind: SuggestionKind::Edit,
            target_event_id: Some("e1".to_owned()),
            payload: SuggestionPayload::Edit(EditPayload {
                title: Some("Better".to_owned()),
                ..EditPayload::default()
            }),
            submitted_by: "author".to_owned(),
            submitted_at: now(),
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            report_count: 0,
            flagged: false,
        };
        let json = suggestion_json(&suggestion);
        assert_eq!(json["id"], "s1");
        assert_eq!(json["timelineId"], "t1");
        assert_eq!(json["state"], "PENDING");
        assert_eq!(json["kind"], "EDIT");
        assert_eq!(json["targetEventId"], "e1");
        assert_eq!(json["payload"]["title"], "Better");
        assert!(json["payload"].get("date").is_none());
    }

    #[test]
    fn viewer_addresses_lose_their_port_and_never_come_from_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(VIEWER_ADDRESS_HEADER, "203.0.113.9:44321".parse().unwrap());
        assert_eq!(client_ip(&headers).as_deref(), Some("203.0.113.9"));

        headers.insert(VIEWER_ADDRESS_HEADER, "[2001:db8::1]:443".parse().unwrap());
        assert_eq!(client_ip(&headers).as_deref(), Some("2001:db8::1"));

        let mut spoofed = HeaderMap::new();
        spoofed.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(client_ip(&spoofed), None);
    }

    #[test]
    fn ip_hashes_are_salted_deterministic_and_opaque() {
        let hash = hash_ip("203.0.113.9", "salt-a");
        assert_eq!(hash, hash_ip("203.0.113.9", "salt-a"));
        assert_ne!(hash, hash_ip("203.0.113.9", "salt-b"));
        assert_ne!(hash, hash_ip("203.0.113.10", "salt-a"));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("203.0.113.9"));
    }

    #[test]
    fn retry_after_points_at_the_end_of_the_fixed_window() {
        assert_eq!(
            retry_after_seconds(&now(), SubmitLimit::AccountHourly),
            3600 - (34 * 60 + 56)
        );
        assert_eq!(
            retry_after_seconds(&now(), SubmitLimit::AccountDaily),
            86_400 - (12 * 3600 + 34 * 60 + 56)
        );
    }

    #[test]
    fn kinds_are_case_insensitive_and_unknown_kinds_are_refused() {
        assert_eq!(parse_kind("append").unwrap(), SuggestionKind::Append);
        assert_eq!(parse_kind(" Edit ").unwrap(), SuggestionKind::Edit);
        assert!(parse_kind("delete").is_err());
    }
}
