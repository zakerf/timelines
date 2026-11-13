//! Collection CRUD and Timeline membership: the owner-authenticated mutations
//! behind the browsing surface (PRD §3, §6 access pattern 4).
//!
//! A Collection's own page and Home both re-render synchronously after a
//! mutation, reusing ticket 09's render → S3 `PUT` → invalidation path.

mod store;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post, put};
use data::Timestamp;
use data::model::Collection;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use crate::publish::generated_id;
use crate::render::{self, is_safe_segment};

/// Mount the Collection routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/collections", post(create_collection))
        .route(
            "/collections/{id}",
            patch(rename_collection).delete(delete_collection),
        )
        .route("/timeline/{id}/collection", put(set_membership))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCollectionRequest {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "parent_collection_id")]
    parent_collection_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameCollectionRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MembershipRequest {
    #[serde(default, alias = "collection_id")]
    collection_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionResponse {
    id: String,
    title: String,
    description: String,
    parent_collection_id: Option<String>,
    created_at: String,
    created_by: String,
}

impl From<&Collection> for CollectionResponse {
    fn from(collection: &Collection) -> Self {
        Self {
            id: collection.id.clone(),
            title: collection.title.clone(),
            description: collection.description.clone(),
            parent_collection_id: collection.parent_collection_id.clone(),
            created_at: collection.created_at.as_str().to_owned(),
            created_by: collection.created_by.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MembershipResponse {
    timeline_id: String,
    collection_id: Option<String>,
}

async fn create_collection(
    State(state): State<AppState>,
    identity: Identity,
    Json(request): Json<CreateCollectionRequest>,
) -> Response {
    match run_create(&state, &identity, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_create(
    state: &AppState,
    identity: &Identity,
    request: CreateCollectionRequest,
) -> Result<Response, CollectionError> {
    let title = require_title(&request.title)?;
    let parent = normalize_id(request.parent_collection_id.as_deref());
    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;

    if let Some(parent_id) = &parent {
        store::get_collection(ddb, table, parent_id)
            .await
            .map_err(CollectionError::Internal)?
            .ok_or_else(|| {
                CollectionError::InvalidRequest(format!(
                    "parent collection {parent_id:?} does not exist"
                ))
            })?;
    }

    let collection = Collection {
        id: generated_id(&title),
        title,
        description: request.description.trim().to_owned(),
        parent_collection_id: parent.clone(),
        created_at: Timestamp::now(),
        created_by: identity.account_id.clone(),
    };
    store::create_collection(ddb, table, &collection)
        .await
        .map_err(CollectionError::Internal)?;

    let paths = collection_paths(collection.parent_collection_id.as_deref(), &collection.id);
    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok(json_response(
        StatusCode::CREATED,
        &CollectionResponse::from(&collection),
    ))
}

async fn rename_collection(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<String>,
    Json(request): Json<RenameCollectionRequest>,
) -> Response {
    match run_rename(&state, &identity, &id, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_rename(
    state: &AppState,
    identity: &Identity,
    id: &str,
    request: RenameCollectionRequest,
) -> Result<Response, CollectionError> {
    if request.title.is_none() && request.description.is_none() {
        return Err(CollectionError::InvalidRequest(
            "send a title and/or description to change".to_owned(),
        ));
    }

    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;
    let mut collection = owned_collection(ddb, table, id, identity).await?;

    if let Some(title) = request.title {
        collection.title = require_title(&title)?;
    }
    if let Some(description) = request.description {
        collection.description = description.trim().to_owned();
    }

    store::update_metadata(
        ddb,
        table,
        &collection.id,
        &collection.title,
        &collection.description,
        &identity.account_id,
    )
    .await
    .map_err(CollectionError::Internal)?;

    let paths = collection_paths(collection.parent_collection_id.as_deref(), &collection.id);
    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok(json_response(
        StatusCode::OK,
        &CollectionResponse::from(&collection),
    ))
}

async fn delete_collection(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<String>,
) -> Response {
    match run_delete(&state, &identity, &id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_delete(
    state: &AppState,
    identity: &Identity,
    id: &str,
) -> Result<Response, CollectionError> {
    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;
    let collection = owned_collection(ddb, table, id, identity).await?;

    let contents = store::list_contents(ddb, table, Some(&collection.id))
        .await
        .map_err(CollectionError::Internal)?;
    if !contents.collections.is_empty() || !contents.timelines.is_empty() {
        return Err(CollectionError::Conflict(
            "a collection must be empty before it can be deleted".to_owned(),
        ));
    }

    store::delete_collection(ddb, table, &collection.id, &identity.account_id)
        .await
        .map_err(CollectionError::Internal)?;

    // The page object no longer has a document; drop it before re-rendering
    // the parent so a reader never gets a stale child entry.
    let own_path = format!("/collection/{}", collection.id);
    render::delete_page(state, &own_path).await;
    let mut paths = vec!["/".to_owned()];
    if let Some(parent_id) = collection.parent_collection_id.as_deref()
        && is_safe_segment(parent_id)
    {
        paths.push(format!("/collection/{parent_id}"));
    }
    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn set_membership(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<String>,
    Json(request): Json<MembershipRequest>,
) -> Response {
    match run_set_membership(&state, &identity, &id, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn run_set_membership(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    request: MembershipRequest,
) -> Result<Response, CollectionError> {
    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(CollectionError::Internal)?
        .ok_or_else(|| CollectionError::NotFound(format!("timeline {timeline_id:?} not found")))?;
    if timeline.owner_id != identity.account_id {
        return Err(CollectionError::Forbidden);
    }

    let new_collection = normalize_id(request.collection_id.as_deref());
    if let Some(collection_id) = &new_collection {
        store::get_collection(ddb, table, collection_id)
            .await
            .map_err(CollectionError::Internal)?
            .ok_or_else(|| {
                CollectionError::InvalidRequest(format!(
                    "collection {collection_id:?} does not exist"
                ))
            })?;
    }

    let old_collection = timeline.collection_id.clone();
    if old_collection == new_collection {
        return Ok(json_response(
            StatusCode::OK,
            &MembershipResponse {
                timeline_id: timeline.id,
                collection_id: new_collection,
            },
        ));
    }

    store::set_timeline_collection(
        ddb,
        table,
        &timeline.id,
        &timeline.created_at,
        new_collection.as_deref(),
        &identity.account_id,
        &Timestamp::now(),
    )
    .await
    .map_err(CollectionError::Internal)?;

    render::rerender_paths(
        state,
        &membership_paths(old_collection.as_deref(), new_collection.as_deref()),
    )
    .await
    .map_err(CollectionError::render)?;

    Ok(json_response(
        StatusCode::OK,
        &MembershipResponse {
            timeline_id: timeline.id,
            collection_id: new_collection,
        },
    ))
}

async fn owned_collection(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    id: &str,
    identity: &Identity,
) -> Result<Collection, CollectionError> {
    let collection = store::get_collection(ddb, table, id)
        .await
        .map_err(CollectionError::Internal)?
        .ok_or_else(|| CollectionError::NotFound(format!("collection {id:?} not found")))?;
    if collection.created_by != identity.account_id {
        return Err(CollectionError::Forbidden);
    }
    Ok(collection)
}

/// Pages a Timeline membership change dirties: Home and both affected
/// collection pages (PRD §4).
fn membership_paths(old: Option<&str>, new: Option<&str>) -> Vec<String> {
    let mut paths = vec!["/".to_owned()];
    for id in [old, new].into_iter().flatten() {
        if is_safe_segment(id) {
            let path = format!("/collection/{id}");
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// Pages a Collection create/rename dirties: Home, its own page, and its
/// parent's child list.
fn collection_paths(parent: Option<&str>, own: &str) -> Vec<String> {
    let mut paths = vec!["/".to_owned()];
    if let Some(parent) = parent
        && is_safe_segment(parent)
    {
        paths.push(format!("/collection/{parent}"));
    }
    if is_safe_segment(own) {
        let own_path = format!("/collection/{own}");
        if !paths.contains(&own_path) {
            paths.push(own_path);
        }
    }
    paths
}

fn require_title(value: &str) -> Result<String, CollectionError> {
    let title = value.trim();
    if title.is_empty() {
        return Err(CollectionError::InvalidRequest(
            "collection title is required".to_owned(),
        ));
    }
    Ok(title.to_owned())
}

fn normalize_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn json_response<T: Serialize>(status: StatusCode, body: &T) -> Response {
    (
        status,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(body),
    )
        .into_response()
}

/// Refusal shapes: malformed request, unknown target, wrong owner, non-empty
/// delete, and store/render failure.
#[derive(Debug)]
enum CollectionError {
    InvalidRequest(String),
    NotFound(String),
    Forbidden,
    Conflict(String),
    Config(ConfigError),
    Internal(String),
}

impl CollectionError {
    fn render(error: render::RenderError) -> Self {
        match error {
            render::RenderError::Config(error) => Self::Config(error),
            render::RenderError::Internal(message) => Self::Internal(message),
        }
    }
}

impl IntoResponse for CollectionError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message).into_response(),
            Self::Forbidden => (StatusCode::FORBIDDEN, "not your collection").into_response(),
            Self::Conflict(message) => (StatusCode::CONFLICT, message).into_response(),
            Self::Config(error) => {
                tracing::error!(%error, "collection mutation refused: configuration incomplete");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "collections are unavailable",
                )
                    .into_response()
            }
            Self::Internal(message) => {
                tracing::error!(%message, "collection mutation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "collection mutation failed",
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_change_renders_home_and_both_collection_pages() {
        assert_eq!(
            membership_paths(Some("old"), Some("new")),
            ["/", "/collection/old", "/collection/new"]
        );
        assert_eq!(
            membership_paths(None, Some("new")),
            ["/", "/collection/new"],
            "removing from ROOT only dirties the new page"
        );
        assert_eq!(
            membership_paths(Some("same"), Some("same")),
            ["/", "/collection/same"],
            "one page when both ends are the same"
        );
        assert_eq!(
            membership_paths(None, None),
            ["/"],
            "uncollected stays Home-only"
        );
    }

    #[test]
    fn collection_create_and_rename_dirty_home_own_and_parent() {
        assert_eq!(collection_paths(None, "new"), ["/", "/collection/new"]);
        assert_eq!(
            collection_paths(Some("parent"), "child"),
            ["/", "/collection/parent", "/collection/child"]
        );
        assert_eq!(
            collection_paths(Some("../x"), "child"),
            ["/", "/collection/child"],
            "unsafe ids never become a page path"
        );
    }

    #[test]
    fn generated_collection_ids_are_path_safe_and_non_empty() {
        let id = generated_id("Elections 2026");
        assert!(id.starts_with("elections-2026-"));
        assert!(is_safe_segment(&id));
        assert!(is_safe_segment(&generated_id("日本語")));
    }

    #[test]
    fn blank_titles_are_refused() {
        assert!(require_title("   ").is_err());
        assert_eq!(require_title("  Elections  ").expect("title"), "Elections");
    }
}
