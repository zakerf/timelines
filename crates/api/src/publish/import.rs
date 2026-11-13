//! Import a Timeline from a research-timeline JSON document.
//!
//! The bulk twin of the interactive publish: deserialize one typed document
//! (the struct mirrors `schema/timeline.schema.json`; the schema is never
//! evaluated at runtime), run the same §8 checklist, then reuse the publish
//! write path: one `TransactWriteItems` guarded by the publish limits, a
//! synchronous re-render, exact-path invalidation, and the HTMX fragment.

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::{
    EventDraft, FieldError, MAX_BODY_BYTES, PublishDraft, PublishError, build, commit_and_respond,
    generated_id, is_exact_date,
};
use crate::AppState;
use crate::auth::Identity;
use data::Timestamp;

/// The whole import document. Required fields are required by the struct.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportDocument {
    title: String,
    description: String,
    events: Vec<ImportEvent>,
}

/// One Event as the document supplies it. `date` is parsed while
/// deserializing so a malformed date is a per-field error like any other.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportEvent {
    date: EventDate,
    title: String,
    description: String,
    sources: Vec<String>,
}

/// An exact calendar day or an ISO-8601 date-time, normalized to a UTC
/// timestamp and the reader-facing day.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EventDate {
    date: String,
    timestamp: Timestamp,
}

impl EventDate {
    fn parse(value: &str) -> Result<Self, String> {
        const MESSAGE: &str = "exact date as YYYY-MM-DD or an ISO-8601 date-time is required";

        let value = value.trim();
        if value.is_empty() {
            return Err(MESSAGE.to_owned());
        }

        if is_exact_date(value) {
            let timestamp = Timestamp::from_date(value).map_err(|_| MESSAGE.to_owned())?;
            return Ok(Self {
                date: value.to_owned(),
                timestamp,
            });
        }

        // The schema allows a seconds- and/or timezone-optional date-time;
        // assume UTC when the input omits them.
        for candidate in [
            value.to_owned(),
            format!("{value}Z"),
            format!("{value}:00Z"),
        ] {
            if let Ok(timestamp) = Timestamp::parse(&candidate) {
                return Ok(Self {
                    date: value.get(..10).unwrap_or(value).to_owned(),
                    timestamp,
                });
            }
        }

        Err(MESSAGE.to_owned())
    }
}

impl<'de> Deserialize<'de> for EventDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Deserialize the body. Every failure is one per-field error, matching the
/// checklist refusal shape.
fn parse_document(bytes: &[u8]) -> Result<ImportDocument, Vec<FieldError>> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let path = error.path().to_string();
        let message = error.into_inner().to_string();
        vec![field_error(&path, &message)]
    })
}

fn field_error(path: &str, message: &str) -> FieldError {
    let path = path.trim_start_matches('.');
    let field = match (path.is_empty(), named_field(message)) {
        // A missing/unknown field is named by serde but sits below the last
        // path segment, so append it.
        (false, Some(name)) => format!("{path}.{name}"),
        (false, None) => path.to_owned(),
        (true, Some(name)) => name.to_owned(),
        (true, None) => "body".to_owned(),
    };
    FieldError {
        field,
        message: message.to_owned(),
    }
}

fn named_field(message: &str) -> Option<&str> {
    for prefix in ["missing field `", "unknown field `", "duplicate field `"] {
        if let Some(rest) = message.strip_prefix(prefix) {
            return rest.split('`').next();
        }
    }
    None
}

/// Flatten the document into the publish draft and the sort-key timestamps,
/// committing Events earliest-first.
fn into_draft(document: ImportDocument) -> (PublishDraft, Vec<Timestamp>) {
    let mut events = document.events;
    events.sort_by(|left, right| left.date.timestamp.cmp(&right.date.timestamp));

    let timestamps = events
        .iter()
        .map(|event| event.date.timestamp.clone())
        .collect();
    let events = events
        .into_iter()
        .map(|event| EventDraft {
            date: event.date.date,
            title: event.title,
            description: event.description,
            sources: event.sources,
            emphasis: None,
        })
        .collect();

    (
        PublishDraft {
            title: document.title,
            description: document.description,
            collection_id: None,
            events,
        },
        timestamps,
    )
}

pub(super) async fn import_timeline(
    State(state): State<AppState>,
    identity: Identity,
    headers: axum::http::HeaderMap,
    request: Request,
) -> Response {
    match run_import(&state, &identity, &headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_import(
    state: &AppState,
    identity: &Identity,
    headers: &axum::http::HeaderMap,
    request: Request,
) -> Result<Response, PublishError> {
    let document = parse_request(request).await?;
    let (draft, timestamps) = into_draft(document);

    let now = Timestamp::now();
    let timeline_id = generated_id(&draft.title);
    let event_ids = draft
        .events
        .iter()
        .map(|event| generated_id(&event.title))
        .collect();

    let mut new = build(
        &draft,
        &identity.account_id,
        timeline_id,
        event_ids,
        now.clone(),
    )
    .map_err(PublishError::Checklist)?;
    for (event, timestamp) in new.events.iter_mut().zip(timestamps) {
        event.timestamp = timestamp;
    }

    let ip_hash = super::ip_hash_for(state, headers).await;
    commit_and_respond(state, new, now, ip_hash.as_deref()).await
}

async fn parse_request(request: Request) -> Result<ImportDocument, PublishError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.contains("application/json") {
        return Err(PublishError::InvalidFields(vec![FieldError {
            field: "body".to_owned(),
            message: "send one application/json document".to_owned(),
        }]));
    }

    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            PublishError::InvalidRequest(format!("could not read the body: {error}"))
        })?;

    parse_document(&bytes).map_err(PublishError::InvalidFields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn document() -> Value {
        json!({
            "title": "Ohio Senate 2026",
            "description": "One subject, told in dated facts.",
            "events": [
                {
                    "date": "2026-02-04",
                    "title": "Candidate filing deadline",
                    "description": "Petitions were due.",
                    "sources": ["https://www.ohiosos.gov/elections/candidates/"]
                },
                {
                    "date": "2026-05-05",
                    "title": "Primary election day",
                    "description": "Nominees were decided.",
                    "sources": ["https://example.org/primary"]
                }
            ]
        })
    }

    fn parse(value: &Value) -> Result<ImportDocument, Vec<FieldError>> {
        parse_document(value.to_string().as_bytes())
    }

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|error| error.field.as_str()).collect()
    }

    fn draft(value: &Value) -> (PublishDraft, Vec<Timestamp>) {
        into_draft(parse(value).expect("valid document"))
    }

    #[test]
    fn a_valid_document_becomes_a_publish_draft() {
        let (draft, timestamps) = draft(&document());

        assert_eq!(draft.title, "Ohio Senate 2026");
        assert_eq!(draft.description, "One subject, told in dated facts.");
        assert_eq!(draft.collection_id, None, "an import is uncollected");
        assert_eq!(draft.events.len(), 2);
        assert_eq!(draft.events[0].date, "2026-02-04");
        assert_eq!(draft.events[0].title, "Candidate filing deadline");
        assert_eq!(
            draft.events[0].sources,
            ["https://www.ohiosos.gov/elections/candidates/"]
        );
        assert_eq!(timestamps.len(), 2);
    }

    #[test]
    fn a_missing_field_is_a_field_error() {
        let mut value = document();
        value.as_object_mut().expect("object").remove("description");

        let errors = parse(&value).expect_err("missing description fails");
        assert_eq!(fields(&errors), ["description"]);
    }

    #[test]
    fn a_missing_nested_field_is_a_nested_field_error() {
        let mut value = document();
        value["events"][0]
            .as_object_mut()
            .expect("object")
            .remove("sources");

        let errors = parse(&value).expect_err("missing sources fails");
        assert_eq!(fields(&errors), ["events[0].sources"]);
    }

    #[test]
    fn a_wrong_type_is_a_field_error() {
        let mut value = document();
        value["title"] = json!(42);

        let errors = parse(&value).expect_err("wrong title type fails");
        assert_eq!(fields(&errors), ["title"]);
    }

    #[test]
    fn a_bad_date_is_a_date_field_error() {
        let mut value = document();
        value["events"][0]["date"] = json!("February 4");

        let errors = parse(&value).expect_err("fuzzy date fails");
        assert_eq!(fields(&errors), ["events[0].date"]);
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        let mut value = document();
        value["coverImage"] = json!({"key": "x"});

        let errors = parse(&value).expect_err("additional properties are rejected");
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].message.contains("unknown field `coverImage`"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn a_bad_source_url_is_a_sources_field_error() {
        let mut value = document();
        value["events"][0]["sources"] = json!(["not a url"]);

        let (draft, _) = into_draft(parse(&value).expect("shape is valid"));
        let now = Timestamp::now();
        let errors = build(
            &draft,
            "account-1",
            "ohio-senate-2026-aaaaaa".to_owned(),
            vec!["e0-aaaaaa".to_owned(), "e1-bbbbbb".to_owned()],
            now,
        )
        .expect_err("a source that is not http(s) fails the checklist");

        assert_eq!(fields(&errors), ["events[0].sources"]);
    }

    #[test]
    fn a_date_only_value_is_normalized_to_midnight_utc() {
        let (_, timestamps) = draft(&document());
        assert_eq!(timestamps[0].as_str(), "2026-02-04T00:00:00.000Z");
        assert_eq!(timestamps[1].as_str(), "2026-05-05T00:00:00.000Z");
    }

    #[test]
    fn an_iso_date_time_is_kept_and_normalized_to_utc() {
        let mut value = document();
        value["events"][0]["date"] = json!("2026-02-04T13:30:00+01:00");

        let (draft, timestamps) = draft(&value);
        assert_eq!(draft.events[0].date, "2026-02-04");
        assert_eq!(timestamps[0].as_str(), "2026-02-04T12:30:00.000Z");
    }

    #[test]
    fn events_are_committed_earliest_first() {
        let mut value = document();
        value["events"] = json!([
            {
                "date": "2026-05-05",
                "title": "Primary election day",
                "description": "Nominees were decided.",
                "sources": ["https://example.org/primary"]
            },
            {
                "date": "2026-02-04",
                "title": "Candidate filing deadline",
                "description": "Petitions were due.",
                "sources": ["https://www.ohiosos.gov/elections/candidates/"]
            }
        ]);

        let (draft, timestamps) = draft(&value);
        assert_eq!(draft.events[0].date, "2026-02-04");
        assert_eq!(draft.events[1].date, "2026-05-05");
        assert!(timestamps[0] < timestamps[1]);
    }

    #[test]
    fn one_build_produces_the_timeline_and_every_event_with_the_event_count() {
        let (draft, timestamps) = draft(&document());
        let now = Timestamp::now();
        let built = build(
            &draft,
            "account-1",
            "ohio-senate-2026-aaaaaa".to_owned(),
            vec!["filing-aaaaaa".to_owned(), "primary-bbbbbb".to_owned()],
            now.clone(),
        )
        .expect("valid import builds");

        assert_eq!(built.timeline.id, "ohio-senate-2026-aaaaaa");
        assert_eq!(built.timeline.owner_id, "account-1");
        assert_eq!(built.timeline.collection_id, None);
        assert_eq!(built.timeline.event_count, 2);
        assert_eq!(built.timeline.updated_at, now);
        assert_eq!(built.events.len(), 2);
        for (event, timestamp) in built.events.iter().zip(timestamps) {
            assert_eq!(event.timeline_id, "ohio-senate-2026-aaaaaa");
            assert_eq!(event.timestamp, timestamp);
            assert_eq!(event.emphasis, data::model::Emphasis::Standard);
        }
    }

    #[tokio::test]
    async fn deserialization_failures_are_400_with_the_field_error_shape() {
        use axum::response::IntoResponse;

        let errors = parse_document(br#"{"title": 5}"#).expect_err("wrong type fails");
        let response = PublishError::InvalidFields(errors).into_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(json["error"], "invalid_request");
        assert_eq!(json["errors"][0]["field"], "title");
    }
}
