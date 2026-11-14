//! Publish a Timeline: the first real mutation and the vertical slice that
//! proves write -> data -> render -> serve.
//!
//! One request runs the whole synchronous path (PRD §5):
//!
//! 1. the publish checklist gates the draft (PRD §8) and failures return `400`
//!    with a per-field error list;
//! 2. TIMELINE + its initial EVENT rows commit in one DynamoDB transaction;
//! 3. the renderer runs in-process and `PUT`s the affected page set to the site
//!    bucket;
//! 4. the exact viewer and object paths are invalidated; and
//! 5. the response is the HTMX fragment for read-your-writes.
//!
//! Locally floci has no CloudFront, so a missing or obviously local
//! distribution id turns invalidation into a log line instead of a failure.

mod checklist;
mod import;
mod store;

pub use checklist::{
    EventDraft, FieldError, NewTimeline, PublishDraft, build, is_exact_date, is_http_source,
    slugify, validate,
};
pub use store::{EVENT_LIMIT_ACTION, EVENTS_PER_DAY, TIMELINE_LIMIT_ACTION};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use data::Timestamp;
use data::model::{Emphasis, Event, Timeline};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Identity, OptionalIdentity};
use crate::config::ConfigError;
use crate::http::{escape_html, hash_ip, html_response, response_with, trusted_client_ip};
use crate::render::{self, RenderError};

/// Largest publish payload accepted. The form is several kilobytes at most.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Mount the publish routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/timeline", get(new_timeline_form).post(publish_timeline))
        .route("/timeline/import", post(import::import_timeline))
        .route("/timeline/{timeline_id}/events", post(append_event))
        .route("/timeline/{timeline_id}/events/new", get(append_event_form))
}

/// JSON body for `POST /api/timeline`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishRequest {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "collection_id")]
    collection_id: Option<String>,
    #[serde(default)]
    events: Vec<EventRequest>,
}

#[derive(Debug, Clone, Deserialize)]
struct EventRequest {
    #[serde(default)]
    date: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    emphasis: Option<String>,
}

impl From<PublishRequest> for PublishDraft {
    fn from(request: PublishRequest) -> Self {
        Self {
            title: request.title,
            description: request.description,
            collection_id: request.collection_id,
            events: request
                .events
                .into_iter()
                .map(|event| EventDraft {
                    date: event.date,
                    title: event.title,
                    description: event.description,
                    sources: event.sources,
                    emphasis: event.emphasis,
                })
                .collect(),
        }
    }
}

/// Form body for the minimal publish page. Events are one per line:
/// `YYYY-MM-DD | Title | source1,source2 | Description`.
#[derive(Debug, Clone, Deserialize)]
struct PublishForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "collectionId", alias = "collection_id")]
    collection_id: Option<String>,
    #[serde(default)]
    events: String,
}

impl From<PublishForm> for PublishDraft {
    fn from(form: PublishForm) -> Self {
        Self {
            title: form.title,
            description: form.description,
            collection_id: form.collection_id,
            events: form
                .events
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(parse_event_line)
                .collect(),
        }
    }
}

fn parse_event_line(line: &str) -> EventDraft {
    let mut parts = line.split('|').map(str::trim);
    let date = parts.next().unwrap_or_default().to_owned();
    let title = parts.next().unwrap_or_default().to_owned();
    let sources = parts
        .next()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::to_owned)
        .collect();
    let description = parts.collect::<Vec<_>>().join(" | ");

    EventDraft {
        date,
        title,
        description,
        sources,
        emphasis: None,
    }
}

/// Owner-authenticated publish. The identity extractor rejects anonymous
/// requests before the body is parsed.
async fn publish_timeline(
    State(state): State<AppState>,
    identity: Identity,
    headers: HeaderMap,
    request: Request,
) -> Response {
    match run_publish(&state, &identity, &headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_publish(
    state: &AppState,
    identity: &Identity,
    headers: &HeaderMap,
    request: Request,
) -> Result<Response, PublishError> {
    let draft = parse_body(request).await?;
    let now = Timestamp::now();
    let timeline_id = generated_id(&draft.title);
    let event_ids = draft
        .events
        .iter()
        .map(|event| generated_id(&event.title))
        .collect();

    let new = build(
        &draft,
        &identity.account_id,
        timeline_id,
        event_ids,
        now.clone(),
    )
    .map_err(PublishError::Checklist)?;

    let ip_hash = ip_hash_for(state, headers).await;
    commit_and_respond(state, new, now, ip_hash.as_deref()).await
}

/// The hashed viewer address for the publish's per-IP limit, when the trusted
/// edge supplied one (PRD §8).
async fn ip_hash_for(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let ip = trusted_client_ip(headers)?;
    Some(hash_ip(&ip, &state.ip_salt().await))
}

/// Commit the rows and finish the synchronous publish path. Shared by the
/// interactive publish and the JSON import.
async fn commit_and_respond(
    state: &AppState,
    new: NewTimeline,
    now: Timestamp,
    ip_hash: Option<&str>,
) -> Result<Response, PublishError> {
    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    store::create_timeline(
        ddb,
        &state.config().table_name,
        &new.timeline,
        &new.events,
        &now,
        ip_hash,
    )
    .await
    .map_err(|error| match error {
        store::StoreError::RateLimited {
            retry_after_seconds,
        } => PublishError::RateLimited(retry_after_seconds),
        store::StoreError::Other(message) => PublishError::Internal(message),
    })?;

    render::rerender(state, &new.timeline)
        .await
        .map_err(PublishError::render)?;

    Ok(created_fragment(&new.timeline, &new.events))
}

/// Read the body as JSON (API clients) or urlencoded form (the publish page).
async fn parse_body(request: Request) -> Result<PublishDraft, PublishError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            PublishError::InvalidRequest(format!("could not read the body: {error}"))
        })?;

    if content_type.contains("application/json") {
        let request: PublishRequest = serde_json::from_slice(&bytes)
            .map_err(|error| PublishError::InvalidRequest(format!("invalid JSON: {error}")))?;
        Ok(request.into())
    } else if content_type.contains("application/x-www-form-urlencoded") {
        let form: PublishForm = serde_urlencoded::from_bytes(&bytes)
            .map_err(|error| PublishError::InvalidRequest(format!("invalid form: {error}")))?;
        Ok(form.into())
    } else {
        Err(PublishError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ))
    }
}

/// Re-run the write path's render for a Timeline another mutation touched.
///
/// Approving a Suggestion dirties the same page set a publish does (PRD §4),
/// so the suggestion module reuses this exact render + upload + invalidation
/// path rather than growing a second one.
pub(crate) async fn rerender_timeline(state: &AppState, timeline: &Timeline) -> Result<(), String> {
    render::rerender(state, timeline)
        .await
        .map_err(|error| format!("{error:?}"))
}

pub(crate) fn generated_id(label: &str) -> String {
    format!(
        "{}-{:06x}",
        slugify(label),
        rand::random::<u32>() & 0x00ff_ffff
    )
}

/// The HTMX fragment the owner gets back: read-your-writes, rendered from the
/// rows just written rather than waiting for cache coherence.
fn created_fragment(timeline: &Timeline, events: &[data::model::Event]) -> Response {
    let id = escape_html(&timeline.id);
    let title = escape_html(&timeline.title);
    let description = escape_html(&timeline.description);
    let event_count = events.len();
    let rows = events
        .iter()
        .map(|event| {
            format!(
                "<li class=\"published__event\" data-event-id=\"{}\">\
                 <time datetime=\"{}\">{}</time>\
                 <strong>{}</strong></li>",
                escape_html(&event.event_id),
                escape_html(&event.date),
                escape_html(&event.date),
                escape_html(&event.title)
            )
        })
        .collect::<String>();

    let html = format!(
        "<article class=\"published\" data-timeline-id=\"{id}\">\
         <p class=\"published__eyebrow\">Published</p>\
         <h2 class=\"published__title\">{title}</h2>\
         <p class=\"published__deck\">{description}</p>\
         <p class=\"published__meta\">{event_count} event(s)</p>\
         <ol class=\"published__events\">{rows}</ol>\
         <p><a href=\"/timeline/{id}\">Open the timeline</a></p>\
         </article>"
    );

    response_with(StatusCode::CREATED, "text/html; charset=utf-8", html)
}

/// Minimal publish page. It is a normal form POST so the browser needs no
/// script; the response is the same fragment the JSON API returns.
async fn new_timeline_form(
    State(_state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
) -> Response {
    if identity.is_none() {
        return Redirect::to("/api/auth/login").into_response();
    }
    response_with(
        StatusCode::OK,
        "text/html; charset=utf-8",
        PUBLISH_FORM_HTML,
    )
}

/// JSON body for `POST /api/timeline/{id}/events`.
#[derive(Debug, Clone, Deserialize)]
struct AppendEventJson {
    #[serde(default)]
    date: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    emphasis: Option<String>,
}

/// Form body for the append page. Sources arrive as one comma-separated field.
#[derive(Debug, Clone, Deserialize)]
struct AppendEventForm {
    #[serde(default)]
    date: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    sources: String,
    #[serde(default)]
    emphasis: Option<String>,
}

/// Owner-authenticated direct append (PRD §5, §6 access pattern 8).
async fn append_event(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
    request: Request,
) -> Response {
    match run_append(&state, &identity, &timeline_id, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_append(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    request: Request,
) -> Result<Response, PublishError> {
    let draft = parse_append_body(request).await?;
    validate_append_event(&draft)?;

    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    let table = &state.config().table_name;
    let timeline = renderer::dynamo::load_timeline(ddb, table, timeline_id)
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                return PublishError::NotFound;
            }
            PublishError::Internal(format!("timeline read failed: {error}"))
        })?;
    if timeline.owner_id != identity.account_id {
        return Err(PublishError::Forbidden);
    }

    let now = Timestamp::now();
    let event = build_appended_event(timeline_id, &identity.account_id, &draft, &now);
    store::append_event(ddb, table, timeline_id, &identity.account_id, &event, &now)
        .await
        .map_err(|error| match error {
            store::StoreError::RateLimited {
                retry_after_seconds,
            } => PublishError::RateLimited(retry_after_seconds),
            store::StoreError::Other(message) => PublishError::Internal(message),
        })?;

    render::rerender(state, &timeline)
        .await
        .map_err(PublishError::render)?;

    Ok(appended_fragment(&timeline, &event))
}

/// The append form page. Owner-gated the same way the publish page is.
async fn append_event_form(
    State(_state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(timeline_id): Path<String>,
) -> Response {
    if identity.is_none() {
        return Redirect::to("/api/auth/login").into_response();
    }
    let html = APPEND_EVENT_FORM_HTML.replace("{timeline_id}", &timeline_id);
    html_response(StatusCode::OK, html)
}

/// `GET` and `POST` both accept JSON and the form page's urlencoded body.
async fn parse_append_body(request: Request) -> Result<EventDraft, PublishError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            PublishError::InvalidRequest(format!("could not read the body: {error}"))
        })?;

    if content_type.contains("application/json") {
        let request: AppendEventJson = serde_json::from_slice(&bytes)
            .map_err(|error| PublishError::InvalidRequest(format!("invalid JSON: {error}")))?;
        Ok(request.into())
    } else if content_type.contains("application/x-www-form-urlencoded") {
        let form: AppendEventForm = serde_urlencoded::from_bytes(&bytes)
            .map_err(|error| PublishError::InvalidRequest(format!("invalid form: {error}")))?;
        Ok(form.into())
    } else {
        Err(PublishError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ))
    }
}

impl From<AppendEventJson> for EventDraft {
    fn from(request: AppendEventJson) -> Self {
        Self {
            date: request.date,
            title: request.title,
            description: request.description,
            sources: request.sources,
            emphasis: request.emphasis,
        }
    }
}

impl From<AppendEventForm> for EventDraft {
    fn from(form: AppendEventForm) -> Self {
        let sources = form
            .sources
            .split(',')
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            date: form.date,
            title: form.title,
            description: form.description,
            sources,
            emphasis: form.emphasis,
        }
    }
}

/// The §8 hard gates for a single appended Event, in the publish error shape.
fn validate_append_event(draft: &EventDraft) -> Result<(), PublishError> {
    let mut errors = Vec::new();
    if !is_exact_date(&draft.date) {
        errors.push(FieldError {
            field: "date".to_owned(),
            message: "exact date as YYYY-MM-DD is required".to_owned(),
        });
    }
    if draft.title.trim().is_empty() {
        errors.push(FieldError {
            field: "title".to_owned(),
            message: "event title is required".to_owned(),
        });
    }
    if !draft.sources.iter().any(|source| is_http_source(source)) {
        errors.push(FieldError {
            field: "sources".to_owned(),
            message: "at least one http(s) source URL is required".to_owned(),
        });
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PublishError::Checklist(errors))
    }
}

/// Build the EVENT row for a direct append. The shape matches the publish
/// checklist's constructor so the two cannot drift.
fn build_appended_event(
    timeline_id: &str,
    owner_id: &str,
    draft: &EventDraft,
    now: &Timestamp,
) -> Event {
    let event_id = generated_id(&draft.title);
    Event {
        timeline_id: timeline_id.to_owned(),
        event_id,
        timestamp: Timestamp::from_date(&draft.date)
            .expect("validate_append_event rejected every non-exact date"),
        date: draft.date.trim().to_owned(),
        title: draft.title.trim().to_owned(),
        description: draft.description.trim().to_owned(),
        sources: draft
            .sources
            .iter()
            .map(|source| source.trim().to_owned())
            .filter(|source| !source.is_empty())
            .collect(),
        emphasis: match draft.emphasis.as_deref() {
            Some("featured") => Emphasis::Featured,
            _ => Emphasis::Standard,
        },
        upvotes: 0,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        created_at: now.clone(),
        created_by: owner_id.to_owned(),
        last_edited_by: None,
        last_edit_suggestion_id: None,
    }
}

/// Read-your-writes fragment for a direct append.
fn appended_fragment(timeline: &Timeline, event: &Event) -> Response {
    let id = escape_html(&timeline.id);
    let title = escape_html(&event.title);
    let date = escape_html(&event.date);
    let html = format!(
        "<article class=\"published\" data-timeline-id=\"{id}\">\
         <p class=\"published__eyebrow\">Event added</p>\
         <h2 class=\"published__title\">{title}</h2>\
         <p class=\"published__meta\">{date}</p>\
         <p><a href=\"/timeline/{id}\">Open the timeline</a></p>\
         </article>"
    );
    html_response(StatusCode::CREATED, html)
}

/// Checklist refusal / malformed body / store failure.
#[derive(Debug)]
enum PublishError {
    InvalidRequest(String),
    InvalidFields(Vec<FieldError>),
    Checklist(Vec<FieldError>),
    NotFound,
    Forbidden,
    RateLimited(u64),
    Config(ConfigError),
    Internal(String),
}

impl PublishError {
    fn render(error: RenderError) -> Self {
        match error {
            RenderError::Config(error) => Self::Config(error),
            RenderError::Internal(message) => Self::Internal(message),
        }
    }
}

impl IntoResponse for PublishError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::InvalidFields(errors) => field_errors_response("invalid_request", errors),
            Self::Checklist(errors) => field_errors_response("checklist_failed", errors),
            Self::NotFound => (StatusCode::NOT_FOUND, "timeline not found").into_response(),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "only the timeline owner can add events",
            )
                .into_response(),
            Self::RateLimited(retry_after_seconds) => {
                let body = serde_json::json!({ "error": "rate_limited" });
                let mut response = response_with(
                    StatusCode::TOO_MANY_REQUESTS,
                    "application/json; charset=utf-8",
                    body.to_string(),
                );
                response.headers_mut().insert(
                    RETRY_AFTER,
                    retry_after_seconds
                        .to_string()
                        .parse()
                        .expect("a decimal second count is a valid header value"),
                );
                response
            }
            Self::Config(error) => {
                tracing::error!(%error, "publish refused: configuration incomplete");
                (StatusCode::SERVICE_UNAVAILABLE, "publish is unavailable").into_response()
            }
            Self::Internal(message) => {
                tracing::error!(%message, "publish failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "publish failed").into_response()
            }
        }
    }
}

/// `400` with the per-field shape every refusal uses (PRD §5).
fn field_errors_response(code: &'static str, errors: Vec<FieldError>) -> Response {
    let body = serde_json::json!({
        "error": code,
        "errors": errors,
    });
    response_with(
        StatusCode::BAD_REQUEST,
        "application/json; charset=utf-8",
        body.to_string(),
    )
}

/// Form page kept in one place; no stylesheet or script dependency.
const PUBLISH_FORM_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Publish a Timeline</title>
  <style>
    :root { color-scheme: light; --paper: #faf8f4; --ink: #17171a; --line: #ddd6c9; --accent: #a63a23; }
    * { box-sizing: border-box; }
    body { margin: 0; background: var(--paper); color: var(--ink); font-family: "Segoe UI", system-ui, sans-serif; }
    main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.35rem 5rem; }
    h1 { font-family: Georgia, serif; font-size: clamp(2rem, 5vw, 3rem); margin: 0 0 0.4rem; }
    p.lede { color: #5c5a60; margin: 0 0 2rem; }
    label { display: block; margin: 1.1rem 0 0.35rem; font-weight: 600; font-size: 0.9rem; }
    input, textarea { width: 100%; padding: 0.6rem 0.7rem; border: 1px solid var(--line); border-radius: 5px; background: #fff; font: inherit; }
    textarea { min-height: 7rem; }
    textarea#events { min-height: 11rem; font-family: ui-monospace, Consolas, monospace; font-size: 0.88rem; }
    .help { margin: 0.35rem 0 0; color: #5c5a60; font-size: 0.82rem; }
    button { margin-top: 1.6rem; padding: 0.65rem 1.2rem; border: 0; border-radius: 5px; background: var(--accent); color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
  </style>
</head>
<body>
<main>
  <h1>Publish a Timeline</h1>
  <p class="lede">Every Event needs an exact date, a title, and at least one <code>http(s)</code> source.</p>
  <form method="post" action="/api/timeline">
    <label for="title">Title</label>
    <input id="title" name="title" required />

    <label for="description">Deck</label>
    <textarea id="description" name="description"></textarea>

    <label for="collectionId">Collection id (optional)</label>
    <input id="collectionId" name="collectionId" />

    <label for="events">Events</label>
    <textarea id="events" name="events" required></textarea>
    <p class="help">One Event per line: <code>YYYY-MM-DD | Title | https://source[,https://source2] | Description</code></p>

    <button type="submit">Publish</button>
  </form>
</main>
</body>
</html>
"#;

/// Append form page; the `{timeline_id}` placeholder is filled with the route's
/// Timeline id at render time. Plain form POST so the browser needs no script.
const APPEND_EVENT_FORM_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Add an event</title>
  <style>
    :root { color-scheme: light; --paper: #faf8f4; --ink: #17171a; --line: #ddd6c9; --accent: #a63a23; }
    * { box-sizing: border-box; }
    body { margin: 0; background: var(--paper); color: var(--ink); font-family: "Segoe UI", system-ui, sans-serif; }
    main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.35rem 5rem; }
    h1 { font-family: Georgia, serif; font-size: clamp(2rem, 5vw, 3rem); margin: 0 0 0.4rem; }
    p.lede { color: #5c5a60; margin: 0 0 2rem; }
    label { display: block; margin: 1.1rem 0 0.35rem; font-weight: 600; font-size: 0.9rem; }
    input, textarea { width: 100%; padding: 0.6rem 0.7rem; border: 1px solid var(--line); border-radius: 5px; background: #fff; font: inherit; }
    textarea { min-height: 6rem; }
    .help { margin: 0.35rem 0 0; color: #5c5a60; font-size: 0.82rem; }
    button { margin-top: 1.6rem; padding: 0.65rem 1.2rem; border: 0; border-radius: 5px; background: var(--accent); color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
  </style>
</head>
<body>
<main>
  <h1>Add an event</h1>
  <p class="lede">An Event needs an exact date, a title, and at least one <code>http(s)</code> source.</p>
  <form method="post" action="/api/timeline/{timeline_id}/events">
    <label for="date">Date</label>
    <input id="date" name="date" placeholder="YYYY-MM-DD" required />

    <label for="title">Title</label>
    <input id="title" name="title" required />

    <label for="sources">Sources</label>
    <input id="sources" name="sources" placeholder="https://source, https://source2" required />

    <label for="description">Description</label>
    <textarea id="description" name="description"></textarea>

    <label for="emphasis">Emphasis (optional)</label>
    <input id="emphasis" name="emphasis" placeholder="standard or featured" />

    <button type="submit">Add event</button>
  </form>
</main>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn event_line(line: &str) -> EventDraft {
        parse_event_line(line)
    }

    #[test]
    fn form_lines_are_date_title_sources_description() {
        let event = event_line(
            "2026-02-04 | Candidate filing deadline | https://a.example, https://b.example | Petitions were due.",
        );
        assert_eq!(event.date, "2026-02-04");
        assert_eq!(event.title, "Candidate filing deadline");
        assert_eq!(event.sources, ["https://a.example", "https://b.example"]);
        assert_eq!(event.description, "Petitions were due.");
    }

    #[test]
    fn a_short_form_line_keeps_missing_fields_empty_for_the_checklist() {
        let event = event_line("2026-02-04 | Title only");
        assert_eq!(event.date, "2026-02-04");
        assert_eq!(event.title, "Title only");
        assert!(event.sources.is_empty());
    }

    #[test]
    fn fragments_escape_user_text() {
        let escaped = escape_html("<script>alert('x')</script>");
        assert!(!escaped.contains("<script>"));
        assert!(escaped.contains("&lt;script&gt;"));
    }
}
