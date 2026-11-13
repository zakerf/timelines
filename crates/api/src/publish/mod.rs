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
mod store;

pub use checklist::{
    EventDraft, FieldError, NewTimeline, PublishDraft, build, is_exact_date, is_http_source,
    slugify, validate,
};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use data::Timestamp;
use data::model::Timeline;
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Identity, OptionalIdentity};
use crate::config::ConfigError;
use crate::render::{self, RenderError};

/// Largest publish payload accepted. The form is several kilobytes at most.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Mount the publish routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/timeline", get(new_timeline_form).post(publish_timeline))
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
    request: Request,
) -> Response {
    match run_publish(&state, &identity, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_publish(
    state: &AppState,
    identity: &Identity,
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

    let NewTimeline { timeline, events } =
        build(&draft, &identity.account_id, timeline_id, event_ids, now)
            .map_err(PublishError::Checklist)?;

    let ddb = state.dynamodb().map_err(PublishError::Config)?;
    store::create_timeline(ddb, &state.config().table_name, &timeline, &events)
        .await
        .map_err(PublishError::Internal)?;

    render::rerender(state, &timeline)
        .await
        .map_err(PublishError::render)?;

    Ok(created_fragment(&timeline, &events))
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

fn response_with(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Body>,
) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("static publish response is valid")
}

/// Checklist refusal / malformed body / store failure.
#[derive(Debug)]
enum PublishError {
    InvalidRequest(String),
    Checklist(Vec<FieldError>),
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
            Self::Checklist(errors) => {
                let body = serde_json::json!({
                    "error": "checklist_failed",
                    "errors": errors,
                });
                response_with(
                    StatusCode::BAD_REQUEST,
                    "application/json; charset=utf-8",
                    body.to_string(),
                )
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

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
