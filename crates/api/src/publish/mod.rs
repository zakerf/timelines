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
use data::model::{Event, Timeline};
use renderer::shell::{FORM_CSS, NOTICE_CSS, PageDocument};
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
    significance: Option<String>,
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
                    significance: event.significance,
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
        significance: None,
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
        return crate::auth::login_redirect("/api/timeline");
    }
    response_with(
        StatusCode::OK,
        "text/html; charset=utf-8",
        publish_form_html(),
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
    significance: Option<String>,
}

/// Form body for the append page. The form repeats `sources` once per source
/// row, so the urlencoded body is read pair by pair instead of through serde.
#[derive(Debug, Clone, Default)]
struct AppendEventForm {
    date: String,
    title: String,
    description: String,
    sources: Vec<String>,
    significance: Option<String>,
}

/// Owner-authenticated direct append (PRD §5, §6 access pattern 8). Anonymous
/// form submissions go through login and come back to the form; API clients
/// still get the `401` contract.
async fn append_event(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(timeline_id): Path<String>,
    request: Request,
) -> Response {
    let is_form = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/x-www-form-urlencoded"));
    let Some(identity) = identity else {
        if is_form {
            return crate::auth::login_redirect(&format!("/api/timeline/{timeline_id}/events/new"));
        }
        return crate::auth::error::AuthError::unauthorized("sign in to continue").into_response();
    };
    match run_append(&state, &identity, &timeline_id, request, is_form).await {
        Ok(response) => response,
        Err(error) if is_form => append_form_error(&error, &timeline_id),
        Err(error) => error.into_response(),
    }
}

async fn run_append(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    request: Request,
    is_form: bool,
) -> Result<Response, PublishError> {
    let draft = parse_append_body(request).await?;
    validate_append_event(&draft)?;

    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    let table = &state.config().table_name;
    let timeline = load_owned_timeline(state, identity, timeline_id).await?;

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

    // A browser form returns to the Timeline it just changed; API clients keep
    // the read-your-writes fragment.
    if is_form {
        return Ok(Redirect::to(&format!("/timeline/{timeline_id}")).into_response());
    }
    Ok(appended_fragment(&timeline, &event))
}

/// Load a Timeline and refuse anyone who is not its owner.
async fn load_owned_timeline(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<Timeline, PublishError> {
    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    let timeline = renderer::dynamo::load_timeline(ddb, &state.config().table_name, timeline_id)
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
    Ok(timeline)
}

/// The append form page. Owner-gated the same way the publish page is.
async fn append_event_form(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(timeline_id): Path<String>,
) -> Response {
    let Some(identity) = identity else {
        return crate::auth::login_redirect(&format!("/api/timeline/{timeline_id}/events/new"));
    };
    if let Err(error) = load_owned_timeline(&state, &identity, &timeline_id).await {
        return append_form_error(&error, &timeline_id);
    }
    let html = append_form_html(&timeline_id);
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
        Ok(parse_append_form(&bytes).into())
    } else {
        Err(PublishError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ))
    }
}

/// Read the append form's repeated `sources` keys as one entry per textbox.
fn parse_append_form(bytes: &[u8]) -> AppendEventForm {
    let mut form = AppendEventForm::default();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        match key.as_ref() {
            "date" => form.date = value.into_owned(),
            "title" => form.title = value.into_owned(),
            "description" => form.description = value.into_owned(),
            "sources" => form.sources.push(value.into_owned()),
            "significance" => form.significance = Some(value.into_owned()),
            _ => {}
        }
    }
    form
}

impl From<AppendEventJson> for EventDraft {
    fn from(request: AppendEventJson) -> Self {
        Self {
            date: request.date,
            title: request.title,
            description: request.description,
            sources: request.sources,
            significance: request.significance,
        }
    }
}

impl From<AppendEventForm> for EventDraft {
    fn from(form: AppendEventForm) -> Self {
        Self {
            date: form.date,
            title: form.title,
            description: form.description,
            sources: form.sources,
            significance: form.significance,
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
        significance: draft
            .significance
            .as_deref()
            .map(str::trim)
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
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

/// The append form gets a readable page for every refusal; JSON clients keep
/// the existing JSON/text contract from [`PublishError::into_response`].
fn append_form_error(error: &PublishError, timeline_id: &str) -> Response {
    let (status, heading, message, details): (StatusCode, &str, String, Vec<&str>) = match error {
        PublishError::NotFound => (
            StatusCode::NOT_FOUND,
            "Timeline not found",
            "This timeline does not exist.".to_owned(),
            Vec::new(),
        ),
        PublishError::Forbidden => (
            StatusCode::FORBIDDEN,
            "Not your timeline",
            "Only the timeline owner can add events.".to_owned(),
            Vec::new(),
        ),
        PublishError::Checklist(errors) | PublishError::InvalidFields(errors) => (
            StatusCode::BAD_REQUEST,
            "Could not add the event",
            "Fix the fields below and try again.".to_owned(),
            errors.iter().map(|error| error.message.as_str()).collect(),
        ),
        PublishError::InvalidRequest(message) => (
            StatusCode::BAD_REQUEST,
            "Could not add the event",
            message.clone(),
            Vec::new(),
        ),
        PublishError::RateLimited(seconds) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Slow down",
            format!("You can add another event in {seconds} seconds."),
            Vec::new(),
        ),
        PublishError::Config(error) => {
            tracing::error!(%error, "append refused: configuration incomplete");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Unavailable",
                "Adding events is unavailable right now.".to_owned(),
                Vec::new(),
            )
        }
        PublishError::Internal(message) => {
            tracing::error!(%message, "append failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The event could not be saved. Try again.".to_owned(),
                Vec::new(),
            )
        }
    };

    let details = if details.is_empty() {
        String::new()
    } else {
        format!(
            "<ul>{}</ul>",
            details
                .iter()
                .map(|detail| format!("<li>{}</li>", escape_html(detail)))
                .collect::<String>()
        )
    };
    let timeline = escape_html(timeline_id);
    let body = format!(
        "<main>\
         <p class=\"eyebrow\">{code}</p><h1>{heading}</h1><p>{message}</p>{details}\
         <p><a href=\"/api/timeline/{timeline}/events/new\">Back to the form</a> \
         &middot; <a href=\"/timeline/{timeline}\">Open the timeline</a></p>\
         </main>",
        code = status.as_u16(),
        heading = escape_html(heading),
        message = escape_html(&message),
    );
    let html = PageDocument::new(heading, &body)
        .stylesheet(NOTICE_CSS)
        .render();
    html_response(status, html)
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

/// Publish-form body. The document shell, tokens, and base form styles come
/// from [`renderer::shell`]; only this page's own layout rules live below.
const PUBLISH_FORM_BODY: &str = r#"<main>
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
</main>"#;

/// Rules specific to the publish page, on top of [`FORM_CSS`].
const PUBLISH_FORM_CSS: &str = r#"
textarea { min-height: 7rem; }
textarea#events { min-height: 11rem; font-family: ui-monospace, Consolas, monospace; font-size: 0.88rem; }
"#;

/// The publish form as a complete document.
fn publish_form_html() -> String {
    PageDocument::new("Publish a Timeline", PUBLISH_FORM_BODY)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .stylesheet(PUBLISH_FORM_CSS)
        .render()
}

/// Append-form body; `{timeline_id}` is filled with the route's Timeline id.
const APPEND_EVENT_FORM_BODY: &str = r#"<main>
  <h1>Add an event</h1>
  <p class="lede">An Event needs an exact date, a title, and at least one <code>http(s)</code> source.</p>
  <form method="post" action="/api/timeline/{timeline_id}/events">
    <label for="date">Date</label>
    <input id="date" name="date" placeholder="YYYY-MM-DD" required />

    <label for="title">Title</label>
    <input id="title" name="title" required />

    <fieldset>
      <legend>Sources</legend>
      <div id="source-rows">
        <div class="source-row">
          <input name="sources" type="url" placeholder="https://source" />
          <button type="button" class="remove-source" hidden>Remove</button>
        </div>
      </div>
      <button type="button" class="add-source" id="add-source">Add another source</button>
      <p class="help">At least one <code>http(s)</code> URL is required.</p>
    </fieldset>

    <label for="description">Description</label>
    <textarea id="description" name="description"></textarea>

    <fieldset>
      <legend>Significance</legend>
      <div class="significance">
        <label><input type="radio" name="significance" value="minor" /> Minor</label>
        <label><input type="radio" name="significance" value="regular" checked /> Regular</label>
        <label><input type="radio" name="significance" value="crucial" /> Crucial</label>
      </div>
    </fieldset>

    <button type="submit">Add event</button>
  </form>
</main>
<script>
  (function () {
    var rows = document.getElementById("source-rows");
    var add = document.getElementById("add-source");

    function sync() {
      var all = rows.querySelectorAll(".source-row");
      all.forEach(function (row, index) {
        row.querySelector(".remove-source").hidden = all.length === 1;
        row.querySelector("input").placeholder =
          index === 0 ? "https://source" : "https://another-source";
      });
    }

    add.addEventListener("click", function () {
      var row = document.createElement("div");
      row.className = "source-row";
      row.innerHTML =
        '<input name="sources" type="url" placeholder="https://another-source" />' +
        '<button type="button" class="remove-source">Remove</button>';
      rows.appendChild(row);
      sync();
      row.querySelector("input").focus();
    });

    rows.addEventListener("click", function (event) {
      var button = event.target.closest(".remove-source");
      if (!button || rows.querySelectorAll(".source-row").length === 1) return;
      button.closest(".source-row").remove();
      sync();
    });

    sync();
  })();
</script>"#;

/// The append form as a complete document. Plain form POST so the browser
/// needs no script for the form itself.
fn append_form_html(timeline_id: &str) -> String {
    let body = APPEND_EVENT_FORM_BODY.replace("{timeline_id}", &escape_html(timeline_id));
    PageDocument::new("Add an event", &body)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .render()
}

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

    #[test]
    fn append_form_keeps_one_source_per_textbox() {
        let form = parse_append_form(
            b"date=2026-02-04&title=Title&sources=https%3A%2F%2Fa.example\
              &sources=https%3A%2F%2Fb.example&significance=crucial",
        );
        let draft: EventDraft = form.into();
        assert_eq!(draft.sources, ["https://a.example", "https://b.example"]);
        assert_eq!(draft.significance.as_deref(), Some("crucial"));
    }

    #[test]
    fn append_form_error_page_explains_an_ownership_refusal() {
        let response = append_form_error(&PublishError::Forbidden, "tl-1");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// A page that goes through the shared shell: one stylesheet, one token
    /// block, the one site header, and no reader navigation beyond it — a form
    /// is a task, not a browsing surface.
    fn assert_shell_page(name: &str, html: &str) {
        assert!(html.starts_with("<!DOCTYPE html>"), "{name} is a document");
        assert!(html.contains("<html lang=\"en\">"), "{name}");
        assert!(html.trim_end().ends_with("</html>"), "{name} closes");
        assert_eq!(
            html.matches("<style").count(),
            1,
            "{name} has exactly one stylesheet"
        );
        assert_eq!(
            html.matches("--paper:").count(),
            1,
            "{name} has exactly one token block"
        );
        assert!(html.contains("var(--accent)"), "{name} uses the tokens");
        // The one site header: site name home, anonymous account area first
        // paint, and the script that upgrades it.
        assert!(
            html.contains("<header class=\"site-header\">"),
            "{name} carries the site header"
        );
        assert!(
            html.contains("<a class=\"site-header__brand\" href=\"/\">Timelines</a>"),
            "{name} links Home from the site name"
        );
        assert!(
            html.contains("href=\"/api/auth/login?next=/\" data-sign-in>Sign in</a>"),
            "{name} offers sign in"
        );
        assert!(
            html.contains("<span class=\"site-header__name\" data-account-name hidden></span>"),
            "{name} bakes no identity"
        );
        assert!(
            html.contains("<script src=\"/assets/app.js\" defer></script>"),
            "{name} loads the site script"
        );
        assert!(
            !html.contains("Recently updated") && !html.contains("Back to all Timelines"),
            "{name} has no reader navigation"
        );
    }

    #[test]
    fn form_pages_render_through_the_shared_shell() {
        assert_shell_page("publish form", &publish_form_html());
        assert_shell_page("append form", &append_form_html("tl-1"));

        let append = append_form_html("tl-1");
        assert!(append.contains("action=\"/api/timeline/tl-1/events\""));
        assert!(append.contains("Add an event"));
    }

    #[tokio::test]
    async fn refusal_pages_render_through_the_shared_shell() {
        let response = append_form_error(&PublishError::Forbidden, "tl-1");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("refusal body is readable");
        let html = String::from_utf8(body.to_vec()).expect("refusal body is utf-8");
        assert_shell_page("append refusal", &html);
        assert!(html.contains("Only the timeline owner can add events."));
        assert!(html.contains("href=\"/timeline/tl-1\""), "a way back");
    }
}
