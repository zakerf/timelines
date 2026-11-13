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
    pub(super) emphasis: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct ReviewRequest {
    #[serde(default)]
    pub(super) note: Option<String>,
}

/// Flat urlencoded form for the "Suggest an edit" page. Sources arrive as one
/// comma-separated field; every content field is optional because an EDIT
/// carries only what the author wants to change.
#[derive(Debug, Clone, Default, Deserialize)]
struct SuggestForm {
    #[serde(default)]
    kind: String,
    #[serde(default, alias = "targetEventId")]
    target_event_id: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    sources: Option<String>,
    #[serde(default)]
    emphasis: Option<String>,
}

impl From<SuggestForm> for SubmitRequest {
    fn from(form: SuggestForm) -> Self {
        let sources = form.sources.map(|sources| {
            sources
                .split(',')
                .map(str::trim)
                .filter(|source| !source.is_empty())
                .map(str::to_owned)
                .collect()
        });
        SubmitRequest {
            kind: form.kind,
            target_event_id: form.target_event_id,
            payload: PayloadRequest {
                date: form.date,
                title: form.title,
                description: form.description,
                sources,
                emphasis: form.emphasis,
            },
        }
    }
}

pub(super) async fn parse_submit_body(request: Request) -> Result<SubmitRequest, SuggestionError> {
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
        serde_json::from_slice(&bytes)
            .map_err(|error| SuggestionError::InvalidRequest(format!("invalid JSON: {error}")))
    } else if content_type.contains("application/x-www-form-urlencoded") {
        let form: SuggestForm = serde_urlencoded::from_bytes(&bytes)
            .map_err(|error| SuggestionError::InvalidRequest(format!("invalid form: {error}")))?;
        Ok(form.into())
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
