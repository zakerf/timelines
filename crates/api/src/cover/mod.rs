//! Cover images: the owner uploads, replaces, or removes the hero image.
//!
//! One request does the whole synchronous path (PRD §9): validate the bytes by
//! magic number → `PutObject` at an immutable `cover/<timelineId>/<uuid>.<ext>`
//! key → write the `coverImage` attribute → re-render the page set → return the
//! hero fragment. Bytes are proxied through the API, never presigned, so the
//! stored `Content-Type` is pinned by the server and a `text/html` object can
//! never land in the media bucket.

mod image;
mod store;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use data::Timestamp;
use data::model::{CoverImage, Timeline};

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use crate::render::{self, RenderError};

use image::MAX_UPLOAD_BYTES;

/// Multipart boundaries and the optional credit ride along with the 4 MiB cap.
const MAX_BODY_BYTES: usize = MAX_UPLOAD_BYTES + 64 * 1024;

/// `credit` is a short attribution line, not a second document. DynamoDB items
/// are capped at 400 KB, so an unbounded text field could turn a bad upload
/// into a failed attribute write.
const MAX_CREDIT_CHARS: usize = 500;

/// Mount the cover routes under `/api`; the path carries the Timeline id.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/timeline/{id}/cover",
        post(upload)
            .delete(remove)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
    )
}

/// Owner-authenticated upload or replacement.
async fn upload(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
    multipart: Multipart,
) -> Response {
    match run_upload(&state, &identity, &timeline_id, multipart).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// Owner-authenticated removal: clear the attribute and drop back to the
/// deterministic gradient.
async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
) -> Response {
    match run_remove(&state, &identity, &timeline_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_upload(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    multipart: Multipart,
) -> Result<Response, CoverError> {
    // Check ownership before reading up to 4 MiB from a stranger.
    let timeline = owned_timeline(state, identity, timeline_id).await?;
    let upload = read_multipart(multipart).await?;
    let image = image::inspect(&upload.bytes).map_err(CoverError::InvalidImage)?;

    let previous = timeline.cover_image.as_ref().map(|cover| cover.key.clone());
    let s3 = state.s3().map_err(CoverError::Config)?;
    let bucket = &state.config().media_bucket;

    let key = image::cover_key(timeline_id, &image::new_object_id(), image.format);
    store::put_media(s3, bucket, &key, upload.bytes, image.format).await?;

    let now = Timestamp::now();
    let cover = CoverImage {
        key: key.clone(),
        width: image.width,
        height: image.height,
        credit: upload.credit,
    };

    if let Err(error) = store::put_cover(
        state.dynamodb().map_err(CoverError::Config)?,
        &state.config().table_name,
        timeline_id,
        &identity.account_id,
        &cover,
        &now,
    )
    .await
    {
        // The attribute is what makes the object reachable; without it the
        // upload is an orphan, so delete it while we still have the key.
        store::delete_media(s3, bucket, &key).await;
        return Err(CoverError::Internal(error));
    }

    let updated = Timeline {
        cover_image: Some(cover),
        updated_at: now,
        ..timeline
    };
    render::rerender(state, &updated).await?;

    if let Some(previous) = previous
        && previous != key
    {
        store::delete_media(s3, bucket, &previous).await;
    }

    Ok(hero_response(
        render::timeline_hero(state, timeline_id).await?,
    ))
}

async fn run_remove(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<Response, CoverError> {
    let timeline = owned_timeline(state, identity, timeline_id).await?;
    let previous = timeline.cover_image.as_ref().map(|cover| cover.key.clone());

    let now = Timestamp::now();
    store::remove_cover(
        state.dynamodb().map_err(CoverError::Config)?,
        &state.config().table_name,
        timeline_id,
        &identity.account_id,
        &now,
    )
    .await?;

    if let Some(key) = previous {
        let s3 = state.s3().map_err(CoverError::Config)?;
        store::delete_media(s3, &state.config().media_bucket, &key).await;
    }

    let updated = Timeline {
        cover_image: None,
        updated_at: now,
        ..timeline
    };
    render::rerender(state, &updated).await?;

    Ok(hero_response(
        render::timeline_hero(state, timeline_id).await?,
    ))
}

/// Load the row and refuse anyone except its owner. A missing row is `404`;
/// a different signed-in account is `403`.
async fn owned_timeline(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<Timeline, CoverError> {
    let ddb = state.dynamodb().map_err(CoverError::Config)?;
    let timeline = renderer::dynamo::load_timeline(ddb, &state.config().table_name, timeline_id)
        .await
        .map_err(CoverError::read)?;

    if timeline.owner_id != identity.account_id {
        return Err(CoverError::Forbidden);
    }
    Ok(timeline)
}

/// Pull the `file` field and the optional `credit` field out of a multipart
/// body. The accepted key is `file` (the form name used by the upload UI).
async fn read_multipart(mut multipart: Multipart) -> Result<Upload, CoverError> {
    let mut bytes = None;
    let mut credit = None;

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name().unwrap_or_default() {
            "file" | "cover" => {
                bytes = Some(field.bytes().await.map_err(multipart_error)?);
            }
            "credit" => {
                let text = field.text().await.map_err(multipart_error)?;
                credit = Some(text);
            }
            _ => {
                // Drain unknown fields so the parser advances cleanly.
                let _ = field.bytes().await;
            }
        }
    }

    let bytes = bytes.ok_or_else(|| {
        CoverError::InvalidRequest("attach the cover as a multipart field named `file`".to_owned())
    })?;
    let credit = normalize_credit(credit);
    if let Some(credit) = &credit
        && credit.chars().count() > MAX_CREDIT_CHARS
    {
        return Err(CoverError::InvalidRequest(format!(
            "credit must be {MAX_CREDIT_CHARS} characters or fewer"
        )));
    }
    Ok(Upload {
        bytes: bytes.to_vec(),
        credit,
    })
}

fn normalize_credit(raw: Option<String>) -> Option<String> {
    raw.map(|credit| credit.trim().to_owned())
        .filter(|credit| !credit.is_empty())
}

/// Preserve axum's `413` for a body above the per-route limit instead of
/// flattening every multipart failure into `400`.
fn multipart_error(error: axum::extract::multipart::MultipartError) -> CoverError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return CoverError::PayloadTooLarge(
            "cover upload is larger than the 4 MiB limit".to_owned(),
        );
    }
    CoverError::InvalidRequest(format!("invalid multipart body: {error}"))
}

fn hero_response(html: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(html))
        .expect("static cover response is valid")
}

struct Upload {
    bytes: Vec<u8>,
    credit: Option<String>,
}

#[derive(Debug)]
enum CoverError {
    InvalidImage(image::InvalidImage),
    InvalidRequest(String),
    PayloadTooLarge(String),
    NotFound,
    Forbidden,
    Config(ConfigError),
    Render(RenderError),
    Internal(String),
}

impl CoverError {
    fn read(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Self::NotFound;
        }
        Self::Internal(format!("timeline read failed: {error}"))
    }
}

impl From<store::StoreError> for CoverError {
    fn from(error: store::StoreError) -> Self {
        Self::Internal(error)
    }
}

impl From<RenderError> for CoverError {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

impl From<ConfigError> for CoverError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl IntoResponse for CoverError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidImage(error) => {
                let status = match error {
                    image::InvalidImage::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
                    _ => StatusCode::BAD_REQUEST,
                };
                (status, error.message()).into_response()
            }
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::PayloadTooLarge(message) => {
                (StatusCode::PAYLOAD_TOO_LARGE, message).into_response()
            }
            Self::NotFound => (StatusCode::NOT_FOUND, "timeline not found").into_response(),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "only the timeline owner can change the cover",
            )
                .into_response(),
            Self::Config(error) => {
                tracing::error!(%error, "cover request refused: configuration incomplete");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cover images are unavailable",
                )
                    .into_response()
            }
            Self::Render(error) => {
                tracing::error!(?error, "cover request failed while rendering");
                (StatusCode::INTERNAL_SERVER_ERROR, "cover request failed").into_response()
            }
            Self::Internal(message) => {
                tracing::error!(%message, "cover request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "cover request failed").into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credit_is_trimmed_and_blank_becomes_absent() {
        assert_eq!(
            normalize_credit(Some("  Photo: Jane  ".to_owned())).as_deref(),
            Some("Photo: Jane")
        );
        assert_eq!(normalize_credit(Some("   ".to_owned())), None);
        assert_eq!(normalize_credit(None), None);
    }
}
