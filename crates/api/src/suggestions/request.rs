//! Request bodies for the suggestion endpoints: JSON or urlencoded forms.

use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::header::CONTENT_TYPE;
use serde::Deserialize;

use super::error::SuggestionError;

/// Largest submission payload accepted.
const MAX_BODY_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubmitRequest {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) target_event_id: Option<String>,
    #[serde(default)]
    pub(super) payload: PayloadRequest,
}

/// Every field is optional because `EDIT` carries only what the author wants to
/// change and `APPEND` supplies the rest. Validation picks the required set.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PayloadRequest {
    #[serde(default)]
    pub(super) date: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) sources: Option<Vec<String>>,
    #[serde(default)]
    pub(super) significance: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct ReviewRequest {
    #[serde(default)]
    pub(super) note: Option<String>,
}

/// Flat urlencoded form for the "Suggest an edit" page. The form repeats
/// `sources` once per source row, so the body is read pair by pair.
///
/// The form is *prefilled* with the Event's current values, so a submitted
/// field is present even when the reader did not touch it. Presence (not
/// emptiness) therefore means "the reader saw this value"; [`super::payload`]
/// diffs it against the stored Event and keeps only what actually changed. That
/// is also what lets a reader clear a field instead of being unable to.
#[derive(Debug, Clone, Default)]
struct SuggestForm {
    kind: String,
    target_event_id: Option<String>,
    date: Option<String>,
    title: Option<String>,
    description: Option<String>,
    sources: Vec<String>,
    significance: Option<String>,
}

/// A parsed submission plus how it arrived: the browser form gets pages back,
/// a JSON client gets the JSON contract.
#[derive(Debug, Clone)]
pub(super) struct SubmitBody {
    pub(super) request: SubmitRequest,
    /// `true` for the urlencoded form the human-facing page posts.
    pub(super) from_form: bool,
}

impl From<SuggestForm> for SubmitRequest {
    fn from(form: SuggestForm) -> Self {
        let sources: Vec<String> = form
            .sources
            .into_iter()
            .map(|source| source.trim().to_owned())
            .filter(|source| !source.is_empty())
            .collect();
        SubmitRequest {
            kind: form.kind,
            target_event_id: form.target_event_id,
            payload: PayloadRequest {
                date: form.date,
                title: form.title,
                description: form.description,
                sources: (!sources.is_empty()).then_some(sources),
                significance: form.significance,
            },
        }
    }
}

/// Keep an optional form field only when the author typed something.
fn non_empty(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_suggest_form(bytes: &[u8]) -> SuggestForm {
    let mut form = SuggestForm::default();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        match key.as_ref() {
            "kind" => form.kind = value.into_owned(),
            "targetEventId" => {
                form.target_event_id = non_empty(value.into_owned());
            }
            // Text fields keep their submitted value, blank included: a blank
            // submitted over a non-blank current value is a proposed clearing.
            "date" => form.date = Some(value.into_owned()),
            "title" => form.title = Some(value.into_owned()),
            "description" => form.description = Some(value.into_owned()),
            "sources" => form.sources.push(value.into_owned()),
            "significance" => form.significance = non_empty(value.into_owned()),
            _ => {}
        }
    }
    form
}

pub(super) async fn parse_submit_body(request: Request) -> Result<SubmitBody, SuggestionError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            SuggestionError::InvalidRequest(format!("could not read the body: {error}"))
        })?;

    if content_type.contains("application/json") {
        let request = serde_json::from_slice(&bytes)
            .map_err(|error| SuggestionError::InvalidRequest(format!("invalid JSON: {error}")))?;
        Ok(SubmitBody {
            request,
            from_form: false,
        })
    } else if content_type.contains("application/x-www-form-urlencoded") {
        Ok(SubmitBody {
            request: parse_suggest_form(&bytes).into(),
            from_form: true,
        })
    } else {
        Err(SuggestionError::InvalidRequest(
            "send application/json or application/x-www-form-urlencoded".to_owned(),
        ))
    }
}

pub(super) async fn parse_review_body(request: Request) -> Result<ReviewRequest, SuggestionError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_form_keeps_every_submitted_value_and_each_source() {
        let form = parse_suggest_form(
            b"kind=EDIT&title=&date=&description=&sources=&sources=https%3A%2F%2Fa.example\
              &significance=",
        );
        let request: SubmitRequest = form.into();
        assert_eq!(request.kind, "EDIT");
        // Prefilled forms submit every field, so a blank is a proposed
        // clearing, not "absent"; the diff against the stored Event decides.
        assert_eq!(request.payload.title.as_deref(), Some(""));
        assert_eq!(request.payload.date.as_deref(), Some(""));
        assert_eq!(request.payload.description.as_deref(), Some(""));
        assert_eq!(
            request.payload.sources,
            Some(vec!["https://a.example".to_owned()])
        );
        assert_eq!(request.payload.significance, None);
    }

    #[test]
    fn a_blank_source_list_means_unchanged() {
        let form = parse_suggest_form(b"kind=EDIT&sources=&sources=");
        let request: SubmitRequest = form.into();
        assert_eq!(request.payload.sources, None);
    }
}
