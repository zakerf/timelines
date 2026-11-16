//! Collection CRUD and Timeline membership: the owner-authenticated mutations
//! behind the browsing surface (PRD §3, §6 access pattern 4).
//!
//! A Collection's own page and Home both re-render synchronously after a
//! mutation, reusing ticket 09's render → S3 `PUT` → invalidation path.

pub(crate) mod store;

use axum::Json;
use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{post, put};
use data::Timestamp;
use data::model::Collection;
use renderer::shell::{NOTICE_CSS, PageDocument};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Identity;
use crate::config::ConfigError;
use crate::http::{escape_html, html_response};
use crate::publish::generated_id;
use crate::render::{self, is_safe_segment};

/// Largest collection form body accepted.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Mount the Collection routes under `/api`.
///
/// The JSON routes stay exactly as they were for clients; the same paths also
/// answer a browser form with a plain POST and a `303` back to the page, so
/// collection management needs no script.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/collections", post(create_collection))
        .route(
            "/collections/{id}",
            post(update_collection_form)
                .patch(rename_collection)
                .delete(delete_collection),
        )
        .route("/collections/{id}/membership", post(membership_form))
        .route("/timeline/{id}/collection", put(set_membership))
}

/// Every Collection, title-sorted, for the pickers on the publish form, the
/// settings page and the management forms.
///
/// A read failure leaves the picker empty rather than failing the page: the
/// caller can still publish or edit, and a dangling id is refused on the write.
pub(crate) async fn picker_options(state: &AppState) -> Vec<Collection> {
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
            tracing::warn!(%error, "collection picker read failed");
            Vec::new()
        }
    }
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
    /// Absent means "leave nesting alone"; `""` means "top level"; any other
    /// value is the new parent id.
    #[serde(default, alias = "parent_collection_id")]
    parent_collection_id: Option<String>,
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
    let collection = create_row(
        state,
        identity,
        &request.title,
        &request.description,
        request.parent_collection_id.as_deref(),
    )
    .await?;

    let paths = collection_paths(collection.parent_collection_id.as_deref(), &collection.id);
    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok(json_response(
        StatusCode::CREATED,
        &CollectionResponse::from(&collection),
    ))
}

/// Create one COLLECTION row after checking the title and that any parent
/// exists. Shared by the JSON create and the publish form's inline create; the
/// caller owns re-rendering.
pub(crate) async fn create_row(
    state: &AppState,
    identity: &Identity,
    title: &str,
    description: &str,
    parent: Option<&str>,
) -> Result<Collection, CollectionError> {
    let title = require_title(title)?;
    let parent = normalize_id(parent);
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
        description: description.trim().to_owned(),
        parent_collection_id: parent,
        created_at: Timestamp::now(),
        created_by: identity.account_id.clone(),
    };
    store::create_collection(ddb, table, &collection)
        .await
        .map_err(CollectionError::Internal)?;
    Ok(collection)
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
    if request.title.is_none()
        && request.description.is_none()
        && request.parent_collection_id.is_none()
    {
        return Err(CollectionError::InvalidRequest(
            "send a title, description, and/or parent to change".to_owned(),
        ));
    }

    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;
    let mut collection = owned_collection(ddb, table, id, identity).await?;
    let old_parent = collection.parent_collection_id.clone();

    if let Some(title) = request.title {
        collection.title = require_title(&title)?;
    }
    if let Some(description) = request.description {
        collection.description = description.trim().to_owned();
    }
    if let Some(parent) = request.parent_collection_id {
        collection.parent_collection_id = normalize_id(Some(&parent));
        if let Some(parent_id) = &collection.parent_collection_id {
            if parent_id == &collection.id {
                return Err(CollectionError::InvalidRequest(
                    "a collection cannot be its own parent".to_owned(),
                ));
            }
            store::get_collection(ddb, table, parent_id)
                .await
                .map_err(CollectionError::Internal)?
                .ok_or_else(|| {
                    CollectionError::InvalidRequest(format!(
                        "parent collection {parent_id:?} does not exist"
                    ))
                })?;
            if nested_under(ddb, table, parent_id, &collection.id).await? {
                return Err(CollectionError::InvalidRequest(
                    "a collection cannot be nested under one of its own children".to_owned(),
                ));
            }
        }
    }

    store::update_metadata(
        ddb,
        table,
        &collection.id,
        &collection.title,
        &collection.description,
        collection.parent_collection_id.as_deref(),
        &identity.account_id,
    )
    .await
    .map_err(CollectionError::Internal)?;

    // Both the old and the new parent list this Collection as a child, the page
    // itself carries the new title and breadcrumb, and every Timeline inside it
    // shows the title in its eyebrow and "more in this collection" list.
    let mut paths = renamed_paths(
        old_parent.as_deref(),
        collection.parent_collection_id.as_deref(),
        &collection.id,
    );
    paths.extend(member_paths(ddb, table, &collection.id).await?);
    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok(json_response(
        StatusCode::OK,
        &CollectionResponse::from(&collection),
    ))
}

/// Is `candidate` (or one of its ancestors) `ancestor_id`? Guards the nest
/// picker and the API against a cycle. Bounded so corrupt data cannot loop.
async fn nested_under(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    candidate: &str,
    ancestor_id: &str,
) -> Result<bool, CollectionError> {
    let mut current = Some(candidate.to_owned());
    for _ in 0..64 {
        let Some(id) = current else {
            return Ok(false);
        };
        if id == ancestor_id {
            return Ok(true);
        }
        current = store::get_collection(ddb, table, &id)
            .await
            .map_err(CollectionError::Internal)?
            .and_then(|collection| collection.parent_collection_id);
    }
    Err(CollectionError::InvalidRequest(
        "the collection hierarchy is too deep or cyclic".to_owned(),
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

/// One urlencoded body from the Collection page's own forms.
#[derive(Debug, Default)]
struct CollectionForm {
    action: String,
    title: Option<String>,
    description: Option<String>,
    /// Present when the form carries the nest picker; `""` is top level.
    parent_collection_id: Option<String>,
    timeline_id: Option<String>,
}

async fn read_form(request: Request) -> Result<CollectionForm, CollectionError> {
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| {
            CollectionError::InvalidRequest(format!("could not read the form: {error}"))
        })?;
    Ok(parse_form(&bytes))
}

fn parse_form(bytes: &[u8]) -> CollectionForm {
    let mut form = CollectionForm::default();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        let value = value.into_owned();
        match key.as_ref() {
            "action" => form.action = value,
            "title" => form.title = Some(value),
            "description" => form.description = Some(value),
            "parentCollectionId" | "parent_collection_id" => {
                form.parent_collection_id = Some(value);
            }
            "timelineId" | "timeline_id" => form.timeline_id = Some(value),
            _ => {}
        }
    }
    form
}

/// `POST /api/collections/{id}` — the Collection page's rename/description/
/// nest form, plus its delete action. A browser gets a `303` back to the page;
/// a refusal is a page explaining why, never JSON.
async fn update_collection_form(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let form = match read_form(request).await {
        Ok(form) => form,
        Err(error) => return error.into_notice(&format!("/collection/{id}")),
    };
    let back = format!("/collection/{id}");

    if form.action == "delete" {
        return match run_delete(&state, &identity, &id).await {
            Ok(_) => Redirect::to("/").into_response(),
            Err(error) => error.into_notice(&back),
        };
    }

    let request = RenameCollectionRequest {
        title: form.title,
        description: form.description,
        parent_collection_id: form.parent_collection_id,
    };
    match run_rename(&state, &identity, &id, request).await {
        Ok(_) => Redirect::to(&back).into_response(),
        Err(error) => error.into_notice(&back),
    }
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
    let new_collection = normalize_id(request.collection_id.as_deref());
    let (_, collection_id) = move_timeline(state, identity, timeline_id, new_collection).await?;

    Ok(json_response(
        StatusCode::OK,
        &MembershipResponse {
            timeline_id: timeline_id.to_owned(),
            collection_id,
        },
    ))
}

/// Move one Timeline to `new_collection` (`None` removes it), re-rendering the
/// affected pages. The Timeline's owner is the only caller who may move it, and
/// the target Collection must exist. Returns the old and new membership.
pub(crate) async fn move_timeline(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    new_collection: Option<String>,
) -> Result<(Option<String>, Option<String>), CollectionError> {
    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;

    let timeline = store::get_timeline(ddb, table, timeline_id)
        .await
        .map_err(CollectionError::Internal)?
        .ok_or_else(|| CollectionError::NotFound(format!("timeline {timeline_id:?} not found")))?;
    if timeline.owner_id != identity.account_id {
        return Err(CollectionError::Forbidden);
    }

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
        return Ok((old_collection, new_collection));
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

    // A move dirties Home, both Collection pages, the Timeline's own page (its
    // eyebrow and "more in this collection" change) and the other members'
    // pages: their related list gains or loses this Timeline.
    let mut paths = membership_paths(old_collection.as_deref(), new_collection.as_deref());
    paths.push(format!("/timeline/{}", timeline.id));
    for collection_id in [old_collection.as_deref(), new_collection.as_deref()]
        .into_iter()
        .flatten()
    {
        paths.extend(member_paths(ddb, table, collection_id).await?);
    }
    paths.sort();
    paths.dedup();

    render::rerender_paths(state, &paths)
        .await
        .map_err(CollectionError::render)?;

    Ok((old_collection, new_collection))
}

/// `POST /api/collections/{id}/membership` — the Collection page's own form.
/// `action=remove` takes the Timeline out; anything else puts it in.
async fn membership_form(
    State(state): State<AppState>,
    identity: Identity,
    Path(collection_id): Path<String>,
    request: Request,
) -> Response {
    let form = match read_form(request).await {
        Ok(form) => form,
        Err(error) => return error.into_notice("/"),
    };
    let Some(timeline_id) = form.timeline_id.clone().filter(|id| !id.trim().is_empty()) else {
        return CollectionError::InvalidRequest("pick a Timeline to move".to_owned())
            .into_notice(&format!("/collection/{collection_id}"));
    };

    let result = run_membership_form(
        &state,
        &identity,
        &collection_id,
        timeline_id.trim(),
        form.action == "remove",
    )
    .await;
    match result {
        Ok(()) => Redirect::to(&format!("/collection/{collection_id}")).into_response(),
        Err(error) => error.into_notice(&format!("/collection/{collection_id}")),
    }
}

async fn run_membership_form(
    state: &AppState,
    identity: &Identity,
    collection_id: &str,
    timeline_id: &str,
    remove: bool,
) -> Result<(), CollectionError> {
    let ddb = state.dynamodb().map_err(CollectionError::Config)?;
    let table = &state.config().table_name;
    // The Collection page is the owner's, so the caller must own the
    // Collection as well as the Timeline they are moving.
    owned_collection(ddb, table, collection_id, identity).await?;
    let new_collection = (!remove).then(|| collection_id.to_owned());
    move_timeline(state, identity, timeline_id, new_collection).await?;
    Ok(())
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

/// A rename or re-nest dirties Home, the page itself, and both parents' child
/// lists: the Collection left one hierarchy and joined another.
fn renamed_paths(old_parent: Option<&str>, new_parent: Option<&str>, own: &str) -> Vec<String> {
    let mut paths = collection_paths(new_parent, own);
    if let Some(old) = old_parent
        && is_safe_segment(old)
    {
        let old_path = format!("/collection/{old}");
        if !paths.contains(&old_path) {
            paths.push(old_path);
        }
    }
    paths
}

/// The Timeline pages inside a Collection: their eyebrow names it and their
/// "more in this collection" list shows its siblings.
async fn member_paths(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    collection_id: &str,
) -> Result<Vec<String>, CollectionError> {
    let timelines = renderer::dynamo::scan_timelines(ddb, table)
        .await
        .map_err(|error| CollectionError::Internal(format!("timeline read failed: {error}")))?;
    Ok(timelines
        .iter()
        .filter(|timeline| timeline.collection_id.as_deref() == Some(collection_id))
        .map(|timeline| format!("/timeline/{}", timeline.id))
        .collect())
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
pub(crate) enum CollectionError {
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

    /// A refusal a browser can read and act on: a page, not a one-line body.
    fn into_notice(self, back_href: &str) -> Response {
        let (status, heading, message) = match self {
            Self::InvalidRequest(message) => (
                StatusCode::BAD_REQUEST,
                "That change was not accepted",
                message,
            ),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, "Not found", message),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "Not yours to change",
                "Only the owner of this Collection can change it.".to_owned(),
            ),
            Self::Conflict(_) => (
                StatusCode::CONFLICT,
                "This collection is not empty",
                "Move or remove its Timelines and child Collections first, then delete it."
                    .to_owned(),
            ),
            Self::Config(error) => {
                tracing::error!(%error, "collection change refused: configuration incomplete");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Collections are unavailable",
                    "That change cannot be made right now. Try again in a moment.".to_owned(),
                )
            }
            Self::Internal(message) => {
                tracing::error!(%message, "collection change failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "That change could not be completed. Try again.".to_owned(),
                )
            }
        };
        html_response(status, collection_notice_page(heading, &message, back_href))
    }
}

/// A small, light notice with the way back to the Collection page.
fn collection_notice_page(heading: &str, message: &str, back_href: &str) -> String {
    let body = format!(
        "<main><h1>{}</h1><p>{}</p><p><a href=\"{}\">Back to the collection</a></p></main>",
        escape_html(heading),
        escape_html(message),
        escape_html(back_href),
    );
    PageDocument::new(heading, &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
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

    #[test]
    fn a_collection_form_keeps_the_fields_the_page_sends() {
        let form =
            parse_form(b"action=rename&title=New+title&description=A+deck&parentCollectionId=");
        assert_eq!(form.action, "rename");
        assert_eq!(form.title.as_deref(), Some("New title"));
        assert_eq!(form.description.as_deref(), Some("A deck"));
        assert_eq!(
            form.parent_collection_id.as_deref(),
            Some(""),
            "an empty parent value means top level, not 'unchanged'"
        );

        let remove = parse_form(b"action=remove&timelineId=t1");
        assert_eq!(remove.action, "remove");
        assert_eq!(remove.timeline_id.as_deref(), Some("t1"));
        assert_eq!(
            remove.parent_collection_id, None,
            "a form without the picker leaves nesting alone"
        );
    }

    #[test]
    fn a_rename_dirty_home_the_page_and_both_parents() {
        assert_eq!(
            renamed_paths(None, Some("new"), "own"),
            ["/", "/collection/new", "/collection/own"]
        );
        assert_eq!(
            renamed_paths(Some("old"), None, "own"),
            ["/", "/collection/own", "/collection/old"],
            "un-nesting still re-renders the parent it left"
        );
        assert_eq!(
            renamed_paths(Some("same"), Some("same"), "own"),
            ["/", "/collection/same", "/collection/own"],
            "one parent page when it did not move"
        );
    }
}
