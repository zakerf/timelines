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
use axum::http::header::{ACCEPT, CONTENT_TYPE, HeaderName, LOCATION, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use data::Timestamp;
use data::model::{Collection, Event, Timeline};
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

/// The publish form's fields exactly as the browser sent them, kept so a
/// refusal can render every value back instead of losing the draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PublishFormValues {
    title: String,
    description: String,
    /// A Collection id, or the sentinel [`NEW_COLLECTION`] when the picker is
    /// on "create a new one".
    collection_id: String,
    /// The title typed into the inline "new collection" field, if any.
    new_collection_title: String,
    events: String,
}

/// The picker value that means "create a Collection and publish into it".
const NEW_COLLECTION: &str = "__new";

impl PublishFormValues {
    /// The draft this form describes. A line that is missing a field still
    /// becomes an Event, so the checklist can name the line that failed. The
    /// inline-create sentinel is not an id, so it never reaches the draft.
    fn draft(&self) -> PublishDraft {
        PublishDraft {
            title: self.title.clone(),
            description: self.description.clone(),
            collection_id: Some(self.collection_id.trim().to_owned())
                .filter(|id| !id.is_empty() && id != NEW_COLLECTION),
            events: parsed_events(&self.events).0,
        }
    }

    /// The new Collection title, if the form asked for one.
    fn new_collection_title(&self) -> Option<&str> {
        let title = self.new_collection_title.trim();
        (!title.is_empty()).then_some(title)
    }
}

/// Read the publish form's fields as sent: unknown keys are ignored and a
/// repeated key keeps its last value, the way the urlencoded body always read.
fn parse_publish_form(bytes: &[u8]) -> PublishFormValues {
    let mut values = PublishFormValues::default();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        match key.as_ref() {
            "title" => values.title = value.into_owned(),
            "description" => values.description = value.into_owned(),
            "collectionId" | "collection_id" => values.collection_id = value.into_owned(),
            "newCollectionTitle" | "new_collection_title" => {
                values.new_collection_title = value.into_owned();
            }
            "events" => values.events = value.into_owned(),
            _ => {}
        }
    }
    values
}

/// The Event drafts a textarea holds plus the 1-based line each came from, so a
/// refusal can say which line to fix. Blank lines are skipped, not numbered.
fn parsed_events(text: &str) -> (Vec<EventDraft>, Vec<usize>) {
    let mut events = Vec::new();
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        events.push(parse_event_line(line));
        lines.push(index + 1);
    }
    (events, lines)
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
///
/// A browser form gets a web flow: `303` to the new read page on success and
/// the form again, values intact, on refusal. API clients keep the JSON
/// contract exactly.
async fn publish_timeline(
    State(state): State<AppState>,
    identity: Identity,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let submission = match parse_body(request).await {
        Ok(submission) => submission,
        Err(error) if is_browser_form(&headers) => {
            // The body never became a draft; still leave the person a form.
            return publish_form_refusal(&state, &PublishFormValues::default(), &error).await;
        }
        Err(error) => return error.into_response(),
    };

    let reply = if submission.form.is_some() {
        PublishReply::Redirect
    } else {
        PublishReply::Fragment
    };

    match run_publish(
        &state,
        &identity,
        &headers,
        &submission.draft,
        submission.form.as_ref(),
        reply,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => match &submission.form {
            Some(values) => publish_form_refusal(&state, values, &error).await,
            None => error.into_response(),
        },
    }
}

async fn run_publish(
    state: &AppState,
    identity: &Identity,
    headers: &HeaderMap,
    draft: &PublishDraft,
    form: Option<&PublishFormValues>,
    reply: PublishReply,
) -> Result<Response, PublishError> {
    let now = Timestamp::now();
    let timeline_id = generated_id(&draft.title);
    let event_ids = draft
        .events
        .iter()
        .map(|event| generated_id(&event.title))
        .collect();

    let mut new = build(
        draft,
        &identity.account_id,
        timeline_id,
        event_ids,
        now.clone(),
    )
    .map_err(PublishError::Checklist)?;

    // The Collection is resolved only once the draft itself is valid, so a
    // refused publish never leaves a freshly created Collection behind.
    new.timeline.collection_id = resolve_collection(state, identity, draft, form).await?;

    let ip_hash = ip_hash_for(state, headers).await;
    commit_and_respond(state, new, now, ip_hash.as_deref(), reply).await
}

/// The Collection the new Timeline joins: an inline-created one wins, otherwise
/// the picked id must still exist. `None` leaves the Timeline uncollected.
///
/// JSON clients still send an id, so the existence check covers them too: a
/// Timeline can never name a Collection that does not exist.
async fn resolve_collection(
    state: &AppState,
    identity: &Identity,
    draft: &PublishDraft,
    form: Option<&PublishFormValues>,
) -> Result<Option<String>, PublishError> {
    if let Some(title) = form.and_then(PublishFormValues::new_collection_title) {
        let collection = crate::collections::create_row(state, identity, title, "", None)
            .await
            .map_err(|error| match error {
                crate::collections::CollectionError::Config(error) => PublishError::Config(error),
                crate::collections::CollectionError::InvalidRequest(message) => {
                    PublishError::InvalidFields(vec![FieldError {
                        field: "newCollectionTitle".to_owned(),
                        message,
                    }])
                }
                other => {
                    PublishError::Internal(format!("inline collection create failed: {other:?}"))
                }
            })?;
        return Ok(Some(collection.id));
    }

    if form.is_some_and(|values| values.collection_id.trim() == NEW_COLLECTION) {
        return Err(PublishError::InvalidFields(vec![FieldError {
            field: "newCollectionTitle".to_owned(),
            message: "a new collection needs a title".to_owned(),
        }]));
    }

    let Some(collection_id) = draft.collection_id.as_deref() else {
        return Ok(None);
    };
    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    let exists =
        crate::collections::store::get_collection(ddb, &state.config().table_name, collection_id)
            .await
            .map_err(PublishError::Internal)?
            .is_some();
    if !exists {
        return Err(PublishError::InvalidFields(vec![FieldError {
            field: "collectionId".to_owned(),
            message: "that collection no longer exists; pick another or create one".to_owned(),
        }]));
    }
    Ok(Some(collection_id.to_owned()))
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
    reply: PublishReply,
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

    Ok(published_response(&new.timeline, &new.events, reply))
}

/// A publish body plus, when it came from the publish page, the raw values a
/// refusal has to render back.
#[derive(Debug, Clone)]
struct PublishSubmission {
    draft: PublishDraft,
    form: Option<PublishFormValues>,
}

/// Read the body as JSON (API clients) or urlencoded form (the publish page).
async fn parse_body(request: Request) -> Result<PublishSubmission, PublishError> {
    let browser = is_browser_form(request.headers());
    let content_type = header_text(request.headers(), CONTENT_TYPE);
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            PublishError::InvalidRequest(format!("could not read the body: {error}"))
        })?;

    if content_type.contains("application/json") {
        let request: PublishRequest = serde_json::from_slice(&bytes)
            .map_err(|error| PublishError::InvalidRequest(format!("invalid JSON: {error}")))?;
        Ok(PublishSubmission {
            draft: request.into(),
            form: None,
        })
    } else if content_type.contains("application/x-www-form-urlencoded") {
        let values = parse_publish_form(&bytes);
        let draft = values.draft();
        Ok(PublishSubmission {
            draft,
            form: browser.then_some(values),
        })
    } else {
        Err(PublishError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ))
    }
}

/// One header value, lowercased, for the content-type/accept checks below.
fn header_text(headers: &HeaderMap, name: HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Whether this request came from the publish page in a browser.
///
/// The content type decides, the way [`append_event`] detects its own form; a
/// caller that asks for `application/json` keeps the API contract even when its
/// body happens to be urlencoded.
fn is_browser_form(headers: &HeaderMap) -> bool {
    header_text(headers, CONTENT_TYPE).contains("application/x-www-form-urlencoded")
        && !header_text(headers, ACCEPT).contains("application/json")
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

/// How a successful publish answers its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishReply {
    /// API clients and the JSON import: `201` plus the read-your-writes
    /// fragment, exactly as before.
    Fragment,
    /// The browser form: `303` to the new Timeline's read page, so the person
    /// lands on the thing they just published.
    Redirect,
}

fn published_response(timeline: &Timeline, events: &[Event], reply: PublishReply) -> Response {
    match reply {
        PublishReply::Fragment => created_fragment(timeline, events),
        PublishReply::Redirect => {
            let location = format!("/timeline/{}", timeline.id);
            let mut response = response_with(StatusCode::SEE_OTHER, "text/html; charset=utf-8", "");
            response.headers_mut().insert(
                LOCATION,
                HeaderValue::from_str(&location).expect("a generated id is a valid Location"),
            );
            response
        }
    }
}

/// The HTMX fragment the owner gets back: read-your-writes, rendered from the
/// rows just written rather than waiting for cache coherence.
fn created_fragment(timeline: &Timeline, events: &[Event]) -> Response {
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
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
) -> Response {
    if identity.is_none() {
        return crate::auth::login_redirect("/api/timeline");
    }
    let collections = collections_for_form(&state).await;
    let html = publish_form_document(&PublishFormValues::default(), &[], None, &collections);
    response_with(StatusCode::OK, "text/html; charset=utf-8", html)
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
/// from [`renderer::shell`]; the copy explains what an Event needs before the
/// person writes one, and a refusal fills the same page with their draft.
fn publish_form_body(
    values: &PublishFormValues,
    errors: &[FieldError],
    notice: Option<&str>,
    collections: &[Collection],
) -> String {
    let (_, event_lines) = parsed_events(&values.events);
    let messages: Vec<(Option<&'static str>, String)> = errors
        .iter()
        .map(|error| form_message(error, &event_lines))
        .collect();

    let title = escape_html(&values.title);
    let description = escape_html(&values.description);
    let new_collection_title = escape_html(&values.new_collection_title);
    let events = escape_html(&values.events);

    let alert = form_alert(&messages, notice);
    let (title_attrs, title_error) = field_state("title", &messages, None);
    let (description_attrs, description_error) = field_state("description", &messages, None);
    let (collection_attrs, collection_error) = field_state("collectionId", &messages, None);
    let (events_attrs, events_error) = field_state("events", &messages, Some("events-help"));
    let (new_title_attrs, new_title_error) =
        field_state("newCollectionTitle", &messages, Some("new-collection-help"));

    // The picker lists what exists, "no collection", and one inline-create
    // option: a person never types an id.
    let creating = !values.new_collection_title.trim().is_empty()
        || values.collection_id.trim() == NEW_COLLECTION;
    let options = collections
        .iter()
        .map(|collection| {
            let selected = !creating && values.collection_id.trim() == collection.id;
            format!(
                "<option value=\"{}\"{}>{}</option>",
                escape_html(&collection.id),
                if selected { " selected" } else { "" },
                escape_html(&collection.title),
            )
        })
        .collect::<String>();
    let none_selected = if !creating && values.collection_id.trim().is_empty() {
        " selected"
    } else {
        ""
    };
    let create_selected = if creating { " selected" } else { "" };
    let new_collection_hidden = if creating { "" } else { " hidden" };

    format!(
        r#"<main>
  <h1>Publish a Timeline</h1>
  <p class="lede">A Timeline has a title, a description, and at least one Event. Every Event needs a date, a title, and at least one <code>http(s)</code> source.</p>
  {alert}
  <form method="post" action="/api/timeline">
    <label for="title">Title</label>
    <input id="title" name="title" required value="{title}"{title_attrs} />
    {title_error}

    <label for="description">Description</label>
    <textarea id="description" name="description"{description_attrs}>
{description}</textarea>
    {description_error}

    <label for="collectionId">Collection (optional)</label>
    <select id="collectionId" name="collectionId" data-collection-picker{collection_attrs}>
      <option value=""{none_selected}>No collection</option>
      {options}
      <option value="{NEW_COLLECTION}"{create_selected}>Create a new collection&hellip;</option>
    </select>
    <p class="help">Collections group Timelines under a shared subject. You can change this later.</p>
    {collection_error}

    <div class="new-collection" data-new-collection{new_collection_hidden}>
      <label for="newCollectionTitle">New collection title</label>
      <input id="newCollectionTitle" name="newCollectionTitle" value="{new_collection_title}"{new_title_attrs} />
      <p class="help" id="new-collection-help">The Collection is created with this Timeline as its first entry.</p>
      {new_title_error}
    </div>

    <label for="events">Events</label>
    <textarea id="events" name="events" required{events_attrs}>
{events}</textarea>
    {events_error}
    <p class="help" id="events-help">One Event per line: four parts separated by <code>|</code>. Required are the exact date as <code>YYYY-MM-DD</code>, a title, and at least one <code>http(s)</code> source.</p>
    <div class="help-block">
      <p>The four parts of a line, in order:</p>
      <ol>
        <li><strong>Date</strong> &mdash; required, written as <code>YYYY-MM-DD</code> (for example <code>2026-02-04</code>).</li>
        <li><strong>Title</strong> &mdash; required, what happened, in a few words.</li>
        <li><strong>Sources</strong> &mdash; required, one or more <code>http(s)</code> links separated by commas.</li>
        <li><strong>Description</strong> &mdash; optional, the detail a reader needs.</li>
      </ol>
      <p>For example:</p>
      <pre><code>2026-02-04 | Candidate filing deadline | https://example.org/filing, https://example.org/calendar | Petitions were due.</code></pre>
    </div>

    <p class="form-actions"><button type="submit">Publish</button> <a href="/">Cancel</a></p>
  </form>
</main>"#
    )
}

/// The summary at the top of the form: one `role="alert"` region that names
/// every problem and links to the control it belongs to.
fn form_alert(messages: &[(Option<&'static str>, String)], notice: Option<&str>) -> String {
    if messages.is_empty() && notice.is_none() {
        return String::new();
    }

    let heading = match notice {
        Some(notice) => format!("<p>{}</p>", escape_html(notice)),
        None => "<p><strong>Could not publish this Timeline.</strong> Fix the highlighted fields and try again.</p>".to_owned(),
    };
    let items = messages
        .iter()
        .map(|(control, message)| match control {
            Some(control) => format!(
                "<li><a href=\"#{control}\">{}</a></li>",
                escape_html(message)
            ),
            None => format!("<li>{}</li>", escape_html(message)),
        })
        .collect::<String>();
    let list = if items.is_empty() {
        String::new()
    } else {
        format!("<ul>{items}</ul>")
    };

    format!("<div class=\"form-error-summary\" role=\"alert\">{heading}{list}</div>")
}

/// The `aria-invalid`/`aria-describedby` attributes for one control and the
/// inline message block that goes with them. `help` is the id of copy that is
/// always shown beside the control, if any.
fn field_state(
    control: &str,
    messages: &[(Option<&'static str>, String)],
    help: Option<&str>,
) -> (String, String) {
    let own = messages
        .iter()
        .filter(|(target, _)| *target == Some(control))
        .map(|(_, message)| message.as_str())
        .collect::<Vec<_>>();

    if own.is_empty() {
        return match help {
            Some(help) => (format!(" aria-describedby=\"{help}\""), String::new()),
            None => (String::new(), String::new()),
        };
    }

    let error_id = format!("{control}-error");
    let described_by = match help {
        Some(help) => format!("{help} {error_id}"),
        None => error_id.clone(),
    };
    let text = own
        .iter()
        .map(|message| format!("<p>{}</p>", escape_html(message)))
        .collect::<String>();

    (
        format!(" aria-invalid=\"true\" aria-describedby=\"{described_by}\""),
        format!("<div class=\"field-error\" id=\"{error_id}\">{text}</div>"),
    )
}

/// Where one refusal belongs on the form and what it says. An Event problem is
/// prefixed with the line that has to change.
fn form_message(error: &FieldError, event_lines: &[usize]) -> (Option<&'static str>, String) {
    match error.field.as_str() {
        "title" => (Some("title"), error.message.clone()),
        "description" => (Some("description"), error.message.clone()),
        "collectionId" | "collection_id" => (Some("collectionId"), error.message.clone()),
        "newCollectionTitle" | "new_collection_title" => {
            (Some("newCollectionTitle"), error.message.clone())
        }
        "events" => (Some("events"), error.message.clone()),
        field => match event_line(field, event_lines) {
            Some(line) => (
                Some("events"),
                format!("Events line {line}: {}", error.message),
            ),
            None => (None, error.message.clone()),
        },
    }
}

/// The 1-based textarea line for an `events[<n>].<field>` error, when the form
/// can map that Event index back to a line.
fn event_line(field_name: &str, event_lines: &[usize]) -> Option<usize> {
    let rest = field_name.strip_prefix("events[")?;
    let (index, _) = rest.split_once(']')?;
    let index = index.parse::<usize>().ok()?;
    event_lines.get(index).copied()
}

/// Rules specific to the publish page, on top of [`FORM_CSS`].
const PUBLISH_FORM_CSS: &str = r#"
textarea { min-height: 7rem; }
.new-collection {
  margin: 0 0 1.1rem;
  padding: 0.2rem 0.95rem 1rem;
  border: 1px solid var(--line);
  border-left: 3px solid var(--accent);
  border-radius: 5px;
  background: #fff;
}
.new-collection label { margin-top: 0.8rem; }
textarea#events { min-height: 11rem; font-family: ui-monospace, Consolas, monospace; font-size: 0.88rem; }
.help-block {
  margin: 1rem 0 0;
  padding: 0.1rem 0 0.1rem 0.9rem;
  border-left: 2px solid var(--line);
  color: var(--muted);
  font-size: 0.86rem;
}
.help-block p { margin: 0.45rem 0 0; }
.help-block ol { margin: 0.35rem 0 0; padding-left: 1.15rem; }
.help-block li { margin-top: 0.2rem; }
pre {
  margin: 0.4rem 0 0;
  padding: 0.7rem 0.8rem;
  overflow-x: auto;
  border: 1px solid var(--line);
  border-radius: 5px;
  background: #fff;
  color: var(--ink);
  font-size: 0.8rem;
  line-height: 1.5;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
"#;

/// The publish form as a complete document. The fresh form and a refusal are
/// the same page; only the values, the messages, and the Collection list differ.
fn publish_form_document(
    values: &PublishFormValues,
    errors: &[FieldError],
    notice: Option<&str>,
    collections: &[Collection],
) -> String {
    let body = publish_form_body(values, errors, notice, collections);
    PageDocument::new("Publish a Timeline", &body)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .stylesheet(PUBLISH_FORM_CSS)
        .render()
}

/// The empty publish form with the picker's Collection list.
#[cfg(test)]
fn publish_form_html(collections: &[Collection]) -> String {
    publish_form_document(&PublishFormValues::default(), &[], None, collections)
}

/// The Collections the picker offers, in title order. A read failure leaves the
/// picker with "No collection" and the inline create: publishing still works,
/// and the refusal path says nothing it cannot back up.
async fn collections_for_form(state: &AppState) -> Vec<Collection> {
    let Some(ddb) = state.dynamodb_opt() else {
        return Vec::new();
    };
    match renderer::dynamo::scan_collections(ddb, &state.config().table_name).await {
        Ok(mut collections) => {
            collections.sort_by(|left, right| {
                left.title
                    .to_lowercase()
                    .cmp(&right.title.to_lowercase())
                    .then_with(|| left.id.cmp(&right.id))
            });
            collections
        }
        Err(error) => {
            tracing::warn!(%error, "publish form: collection read failed");
            Vec::new()
        }
    }
}

/// A refused browser publish: the form again with every submitted value kept,
/// a message next to the field that failed, and the status an API client sees.
async fn publish_form_refusal(
    state: &AppState,
    values: &PublishFormValues,
    error: &PublishError,
) -> Response {
    let collections = collections_for_form(state).await;
    let (status, html) = publish_refusal_document(values, error, &collections);
    html_response(status, html)
}

/// The refusal page itself: status, copied values, inline messages.
fn publish_refusal_document(
    values: &PublishFormValues,
    error: &PublishError,
    collections: &[Collection],
) -> (StatusCode, String) {
    let (status, notice, errors): (StatusCode, Option<String>, Vec<FieldError>) = match error {
        PublishError::Checklist(errors) | PublishError::InvalidFields(errors) => {
            (StatusCode::BAD_REQUEST, None, errors.clone())
        }
        PublishError::InvalidRequest(message) => {
            (StatusCode::BAD_REQUEST, Some(message.clone()), Vec::new())
        }
        PublishError::RateLimited(retry_after_seconds) => (
            StatusCode::TOO_MANY_REQUESTS,
            Some(format!(
                "You can publish another Timeline in {retry_after_seconds} seconds."
            )),
            Vec::new(),
        ),
        PublishError::Forbidden => (
            StatusCode::FORBIDDEN,
            Some("Only the timeline owner can publish here.".to_owned()),
            Vec::new(),
        ),
        PublishError::NotFound => (
            StatusCode::NOT_FOUND,
            Some("That Timeline does not exist.".to_owned()),
            Vec::new(),
        ),
        PublishError::Config(error) => {
            tracing::error!(%error, "publish refused: configuration incomplete");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("Publishing is unavailable right now. Try again later.".to_owned()),
                Vec::new(),
            )
        }
        PublishError::Internal(message) => {
            tracing::error!(%message, "publish failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(
                    "The Timeline could not be saved. Your draft is still here; try again."
                        .to_owned(),
                ),
                Vec::new(),
            )
        }
    };

    let html = publish_form_document(values, &errors, notice.as_deref(), collections);
    (status, html)
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
    use axum::http::header;

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
        assert_shell_page("publish form", &publish_form_html(&[]));
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

    async fn body_of(response: Response) -> String {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the body is readable");
        String::from_utf8(body.to_vec()).expect("the body is utf-8")
    }

    /// A draft with one good line and one line whose date cannot be parsed.
    fn refused_form() -> (PublishFormValues, Vec<FieldError>) {
        let values = PublishFormValues {
            title: "Ohio <Senate> 2026".to_owned(),
            description: "Key dates, \"quoted\".".to_owned(),
            collection_id: "elections-2026".to_owned(),
            new_collection_title: String::new(),
            events: "2026-02-04 | Filing deadline | https://example.org/a | Petitions were due.\n\
                     February 4 | Broken line | https://example.org/b | No exact date."
                .to_owned(),
        };
        let errors = validate(&values.draft()).expect_err("the second line has no exact date");
        (values, errors)
    }

    fn sample_timeline() -> (Timeline, Vec<Event>) {
        let draft = PublishDraft {
            title: "Ohio Senate 2026".to_owned(),
            description: "Key dates.".to_owned(),
            collection_id: None,
            events: vec![EventDraft::new(
                "2026-02-04",
                "Filing deadline",
                "Petitions were due.",
                vec!["https://example.org/a".to_owned()],
            )],
        };
        let now = Timestamp::parse("2026-11-15T09:00:00Z").expect("timestamp");
        let new = build(
            &draft,
            "account-1",
            "ohio-senate-2026-abc123".to_owned(),
            vec!["filing-deadline-aaaaaa".to_owned()],
            now,
        )
        .expect("a valid draft builds");
        (new.timeline, new.events)
    }

    fn headers_of(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                *name,
                HeaderValue::from_str(value).expect("a test header value is valid"),
            );
        }
        headers
    }

    /// A Collection row for the picker.
    fn collection(id: &str, title: &str) -> Collection {
        Collection {
            id: id.to_owned(),
            title: title.to_owned(),
            description: String::new(),
            parent_collection_id: None,
            created_at: Timestamp::from_date("2026-01-01").expect("date"),
            created_by: "owner".to_owned(),
        }
    }

    #[test]
    fn the_picker_offers_existing_collections_none_and_inline_create() {
        let html = publish_form_html(&[
            collection("elections-2026", "Elections 2026"),
            collection("ohio", "Ohio"),
        ]);

        assert!(
            html.contains(
                "<select id=\"collectionId\" name=\"collectionId\" data-collection-picker"
            ),
            "a picker, not an id field: {html}"
        );
        assert!(
            html.contains("<option value=\"\" selected>No collection</option>"),
            "leaving it uncollected is the default choice"
        );
        assert!(
            html.contains("<option value=\"elections-2026\">Elections 2026</option>"),
            "existing Collections are listed by title"
        );
        assert!(
            html.contains("<option value=\"__new\">Create a new collection"),
            "a person can create one inline"
        );
        assert!(
            html.contains("name=\"newCollectionTitle\""),
            "the inline create field exists"
        );
        assert!(
            html.contains("data-new-collection hidden"),
            "the inline field stays closed until it is chosen"
        );
        assert!(
            !html.contains("Collection id (optional)"),
            "no raw id is ever typed"
        );
    }

    #[test]
    fn an_inline_create_keeps_its_title_and_the_picker_stays_on_create() {
        let values = PublishFormValues {
            title: "Ohio Senate 2026".to_owned(),
            collection_id: NEW_COLLECTION.to_owned(),
            new_collection_title: "Elections 2026".to_owned(),
            events: "2026-02-04 | Filing deadline | https://example.org/a | ok".to_owned(),
            ..PublishFormValues::default()
        };
        let html = publish_form_document(&values, &[], None, &[]);

        assert!(
            html.contains("value=\"Elections 2026\""),
            "the typed title survives a refusal: {html}"
        );
        assert!(
            html.contains("<option value=\"__new\" selected>"),
            "the picker still says create"
        );
        assert!(
            !html.contains("data-new-collection hidden"),
            "the inline field stays open"
        );
    }

    #[test]
    fn a_refused_form_keeps_every_submitted_value() {
        let (values, errors) = refused_form();
        let html = publish_form_document(
            &values,
            &errors,
            None,
            &[collection("elections-2026", "Elections 2026")],
        );

        assert!(
            html.contains("value=\"Ohio &lt;Senate&gt; 2026\""),
            "the title comes back, escaped"
        );
        assert!(
            html.contains("Key dates, &quot;quoted&quot;."),
            "the description comes back"
        );
        assert!(
            html.contains("<option value=\"elections-2026\" selected>Elections 2026</option>"),
            "the picked collection is still selected: {html}"
        );
        assert!(
            html.contains(
                "2026-02-04 | Filing deadline | https://example.org/a | Petitions were due."
            ),
            "the first raw line survives"
        );
        assert!(
            html.contains("February 4 | Broken line | https://example.org/b | No exact date."),
            "a line that did not validate still comes back verbatim"
        );
        assert_eq!(values.draft().events.len(), 2, "both lines are events");
    }

    #[test]
    fn a_bad_event_line_names_its_line_next_to_the_events_field() {
        let (values, errors) = refused_form();
        let html = publish_form_document(&values, &errors, None, &[]);

        assert!(html.contains("role=\"alert\""), "the summary is announced");
        assert!(
            html.contains("Events line 2: exact date as YYYY-MM-DD is required"),
            "the message names the line that failed: {html}"
        );
        assert!(
            html.contains("<li><a href=\"#events\">Events line 2:"),
            "the summary points at the control"
        );
        assert!(
            html.contains("<div class=\"field-error\" id=\"events-error\">"),
            "the message sits next to the Events textarea"
        );
        assert!(
            html.contains(
                "id=\"events\" name=\"events\" required aria-invalid=\"true\" \
                 aria-describedby=\"events-help events-error\""
            ),
            "the textarea owns the error"
        );
        assert!(
            !html.contains("id=\"title-error\""),
            "a valid title is not blamed"
        );
    }

    #[test]
    fn a_blank_title_is_reported_next_to_the_title_field() {
        let values = PublishFormValues {
            title: String::new(),
            description: "Keep me".to_owned(),
            collection_id: "elections-2026".to_owned(),
            new_collection_title: String::new(),
            events: "2026-02-04 | Filing deadline | https://example.org/a | ok".to_owned(),
        };
        let errors = validate(&values.draft()).expect_err("a blank title fails");
        let html = publish_form_document(&values, &errors, None, &[]);

        assert!(
            html.contains(
                "<input id=\"title\" name=\"title\" required value=\"\" \
                 aria-invalid=\"true\" aria-describedby=\"title-error\" />"
            ),
            "the title input is marked invalid and described: {html}"
        );
        assert!(html.contains("<div class=\"field-error\" id=\"title-error\">"));
        assert!(html.contains("timeline title is required"));
        assert!(
            !html.contains("aria-invalid=\"true\" aria-describedby=\"events-help events-error\""),
            "the events textarea is not blamed"
        );
    }

    #[test]
    fn a_refused_form_cannot_inject_markup_back_into_the_page() {
        let values = PublishFormValues {
            title: "\"><script>alert(1)</script>".to_owned(),
            description: "</textarea><script>alert(2)</script>".to_owned(),
            collection_id: "\" onfocus=\"alert(3)".to_owned(),
            new_collection_title: "\"><script>alert(5)</script>".to_owned(),
            events: "</textarea><script>alert(4)</script>".to_owned(),
        };
        let html = publish_form_document(&values, &[], None, &[]);
        assert!(!html.contains("<script>"), "{html}");
        assert_shell_page("publish form with hostile values", &html);
    }

    #[test]
    fn a_rate_limited_browser_publish_keeps_the_draft_and_says_why() {
        let (values, _) = refused_form();
        let (status, html) = publish_refusal_document(&values, &PublishError::RateLimited(30), &[]);
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        assert_shell_page("publish refusal", &html);
        assert!(html.contains("You can publish another Timeline in 30 seconds."));
        assert!(
            html.contains("Ohio &lt;Senate&gt; 2026"),
            "the draft is still on the page"
        );
        assert!(
            !html.contains("type=\"hidden\""),
            "no timeline id is pre-generated for a retry"
        );
    }

    #[test]
    fn a_browser_publish_redirects_to_the_new_timeline() {
        let (timeline, events) = sample_timeline();
        let response = published_response(&timeline, &events, PublishReply::Redirect);

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[LOCATION],
            "/timeline/ohio-senate-2026-abc123"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn an_api_publish_still_gets_the_created_fragment() {
        let (timeline, events) = sample_timeline();
        let response = published_response(&timeline, &events, PublishReply::Fragment);

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let html = body_of(response).await;
        assert!(html.contains("data-timeline-id=\"ohio-senate-2026-abc123\""));
        assert!(html.contains("Open the timeline"));
    }

    #[test]
    fn only_a_urlencoded_post_without_a_json_accept_is_the_browser_form() {
        assert!(is_browser_form(&headers_of(&[
            (CONTENT_TYPE.as_str(), "application/x-www-form-urlencoded"),
            (ACCEPT.as_str(), "text/html,application/xhtml+xml"),
        ])));
        assert!(
            !is_browser_form(&headers_of(&[
                (CONTENT_TYPE.as_str(), "application/x-www-form-urlencoded"),
                (ACCEPT.as_str(), "application/json"),
            ])),
            "a client that asks for JSON keeps the JSON contract"
        );
        assert!(!is_browser_form(&headers_of(&[(
            CONTENT_TYPE.as_str(),
            "application/json"
        )])));
        assert!(!is_browser_form(&HeaderMap::new()));
    }

    #[test]
    fn the_form_explains_the_event_fields_and_the_date_format() {
        let html = publish_form_html(&[]);

        assert!(
            html.contains("<label for=\"description\">Description</label>"),
            "the domain word, not Deck"
        );
        assert!(!html.contains(">Deck<"));
        assert!(html.contains("YYYY-MM-DD"), "the date format is stated");
        assert!(html.contains("2026-02-04"), "a whole example line is shown");
        for part in ["Date", "Title", "Sources", "Description"] {
            assert!(
                html.contains(&format!("<strong>{part}</strong>")),
                "{part} is explained in plain language"
            );
        }
        assert!(html.contains("Cancel"), "the form can always be abandoned");
        assert!(
            html.contains("<a href=\"/\">Cancel</a>"),
            "Cancel goes back to where the person came from"
        );
        assert_shell_page("publish form", &html);
    }

    #[test]
    fn a_fresh_form_carries_no_error_chrome() {
        let html = publish_form_html(&[]);
        assert!(!html.contains("role=\"alert\""));
        assert!(!html.contains("class=\"field-error\""));
        assert!(!html.contains("id=\"title-error\""));
        assert!(!html.contains("aria-describedby=\"events-help events-error\""));
        assert!(
            html.contains("aria-describedby=\"events-help\""),
            "the Events copy is still attached to the textarea"
        );
    }
}
