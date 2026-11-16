//! The owner's one settings surface for a Timeline: details, cover, visibility.
//!
//! A published Timeline is a static page shared by every reader, so this page
//! is rendered by the API for its owner only (never cached as a site page). Every
//! action follows the same shape as the other write paths: validate → write →
//! re-render the affected pages → `303` back with a one-line outcome.
//!
//! Unpublishing keeps the Timeline's row, Events, history, and cover; it removes
//! the page and the list entries. Deleting removes the row and its content and
//! says so before it acts. Both are owner-only, and neither is reachable from a
//! static page until `/assets/app.js` reveals the settings link.

use aws_sdk_dynamodb::types::{AttributeValue, DeleteRequest, WriteRequest};
use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use data::Timestamp;
use data::model::{Collection, Timeline};
use renderer::shell::{FORM_CSS, NOTICE_CSS, PageDocument};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Identity, OptionalIdentity};
use crate::config::ConfigError;
use crate::http::{escape_html, html_response};
use crate::render::{self, RenderError, is_safe_segment};

/// Largest settings/multipart-adjacent form body accepted.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Mount the owner settings routes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/timeline/{id}/settings",
        get(settings_page).post(settings_action),
    )
}

#[derive(Debug, Default, Deserialize)]
struct SettingsQuery {
    saved: Option<String>,
}

/// `GET /api/timeline/{id}/settings` — the owner's page.
async fn settings_page(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(timeline_id): Path<String>,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let Some(identity) = identity else {
        return crate::auth::login_redirect(&format!("/api/timeline/{timeline_id}/settings"));
    };
    match load_owned(&state, &identity, &timeline_id).await {
        Ok(timeline) => {
            let collections = crate::collections::picker_options(&state).await;
            html_response(
                StatusCode::OK,
                settings_html(
                    &timeline,
                    &collections,
                    flash_for(query.saved.as_deref()),
                    None,
                ),
            )
        }
        Err(error) => error.into_notice(),
    }
}

/// `POST /api/timeline/{id}/settings` — every form on the page posts here with
/// an `action`: `save`, `unpublish`, `publish`, `delete`, or `remove-cover`.
async fn settings_action(
    State(state): State<AppState>,
    identity: Identity,
    Path(timeline_id): Path<String>,
    request: Request,
) -> Response {
    let form = match read_form(request).await {
        Ok(form) => form,
        Err(message) => {
            return refresh_with_error(
                &state,
                &identity,
                &timeline_id,
                StatusCode::BAD_REQUEST,
                &message,
            )
            .await;
        }
    };

    match form.action.as_str() {
        "save" => save_details(&state, &identity, &timeline_id, form).await,
        "unpublish" => set_visibility(&state, &identity, &timeline_id, false).await,
        "publish" => set_visibility(&state, &identity, &timeline_id, true).await,
        "delete" => delete_timeline(&state, &identity, &timeline_id).await,
        "remove-cover" => remove_cover(&state, &identity, &timeline_id).await,
        other => {
            refresh_with_error(
                &state,
                &identity,
                &timeline_id,
                StatusCode::BAD_REQUEST,
                &format!("unknown settings action {other:?}"),
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async fn save_details(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    form: SettingsForm,
) -> Response {
    let title = form.title.trim();
    if title.is_empty() {
        return refresh_with_error(
            state,
            identity,
            timeline_id,
            StatusCode::BAD_REQUEST,
            "A Timeline needs a title.",
        )
        .await;
    }

    let timeline = match load_owned(state, identity, timeline_id).await {
        Ok(timeline) => timeline,
        Err(error) => return error.into_notice(),
    };

    let new_collection = form
        .collection_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);

    if let Err(error) = store::update_details(
        state,
        identity,
        timeline_id,
        title,
        form.description.trim(),
        &Timestamp::now(),
    )
    .await
    {
        return internal_notice(state, identity, timeline_id, error).await;
    }

    // Membership changes re-render their own pages; the metadata change still
    // needs the Timeline page and the lists.
    if new_collection != timeline.collection_id
        && let Err(error) =
            crate::collections::move_timeline(state, identity, timeline_id, new_collection).await
    {
        return refresh_with_error(
            state,
            identity,
            timeline_id,
            StatusCode::BAD_REQUEST,
            &format!("The collection was not changed: {error:?}"),
        )
        .await;
    }

    let mut paths = vec![
        format!("/timeline/{timeline_id}"),
        "/".to_owned(),
        "/timelines".to_owned(),
    ];
    for collection in [
        timeline.collection_id.as_deref(),
        form.collection_id.as_deref(),
    ] {
        if let Some(collection) = collection
            && is_safe_segment(collection)
        {
            paths.push(format!("/collection/{collection}"));
        }
    }
    if let Err(error) = render::rerender_paths(state, &paths).await {
        return render_notice(error);
    }

    settings_redirect(timeline_id, "details")
}

async fn set_visibility(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    published: bool,
) -> Response {
    let timeline = match load_owned(state, identity, timeline_id).await {
        Ok(timeline) => timeline,
        Err(error) => return error.into_notice(),
    };
    if timeline.published == published {
        return settings_redirect(
            timeline_id,
            if published {
                "published"
            } else {
                "unpublished"
            },
        );
    }

    if let Err(error) =
        store::set_published(state, identity, timeline_id, published, &Timestamp::now()).await
    {
        return internal_notice(state, identity, timeline_id, error).await;
    }

    let mut paths = vec!["/".to_owned(), "/timelines".to_owned()];
    if let Some(collection) = timeline.collection_id.as_deref()
        && is_safe_segment(collection)
    {
        paths.push(format!("/collection/{collection}"));
    }
    // Publishing renders the page; unpublishing removes the object instead.
    if published {
        paths.push(format!("/timeline/{timeline_id}"));
    } else {
        render::delete_page(state, &format!("/timeline/{timeline_id}")).await;
    }
    if let Err(error) = render::rerender_paths(state, &paths).await {
        return render_notice(error);
    }

    settings_redirect(
        timeline_id,
        if published {
            "published"
        } else {
            "unpublished"
        },
    )
}

async fn delete_timeline(state: &AppState, identity: &Identity, timeline_id: &str) -> Response {
    let timeline = match load_owned(state, identity, timeline_id).await {
        Ok(timeline) => timeline,
        Err(error) => return error.into_notice(),
    };

    if let Err(error) = store::delete_contents(state, identity, timeline_id).await {
        return internal_notice(state, identity, timeline_id, error).await;
    }

    // The media object is unreachable once the row is gone; delete it too.
    if let Some(cover) = timeline.cover_image.as_ref()
        && let Ok(s3) = state.s3()
    {
        crate::cover::store::delete_media(s3, &state.config().media_bucket, &cover.key).await;
    }

    render::delete_page(state, &format!("/timeline/{timeline_id}")).await;

    let mut paths = vec!["/".to_owned(), "/timelines".to_owned()];
    if let Some(collection) = timeline.collection_id.as_deref()
        && is_safe_segment(collection)
    {
        paths.push(format!("/collection/{collection}"));
    }
    if let Err(error) = render::rerender_paths(state, &paths).await {
        return render_notice(error);
    }

    // The settings page's own reason to exist is gone; Home is the honest
    // destination, and the Timeline's route now serves the 404 page.
    Redirect::to("/").into_response()
}

async fn remove_cover(state: &AppState, identity: &Identity, timeline_id: &str) -> Response {
    match crate::cover::run_remove(state, identity, timeline_id).await {
        Ok(()) => settings_redirect(timeline_id, "cover-removed"),
        Err(error) => {
            refresh_with_error(
                state,
                identity,
                timeline_id,
                StatusCode::BAD_REQUEST,
                &error.browser_message(),
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// `303` back to the owner's settings page, with the outcome it should show.
/// The cover routes use it too: a browser upload/removal lands here.
pub(crate) fn settings_redirect(timeline_id: &str, saved: &str) -> Response {
    let location = format!("/api/timeline/{timeline_id}/settings?saved={saved}");
    (StatusCode::SEE_OTHER, [(LOCATION, location)], ()).into_response()
}

/// Render the settings page again with a line explaining a refusal.
async fn refresh_with_error(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    status: StatusCode,
    message: &str,
) -> Response {
    match load_owned(state, identity, timeline_id).await {
        Ok(timeline) => {
            let collections = crate::collections::picker_options(state).await;
            html_response(
                status,
                settings_html(&timeline, &collections, None, Some(message)),
            )
        }
        Err(error) => error.into_notice(),
    }
}

async fn internal_notice(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    message: String,
) -> Response {
    tracing::error!(%message, "timeline settings write failed");
    refresh_with_error(
        state,
        identity,
        timeline_id,
        StatusCode::INTERNAL_SERVER_ERROR,
        "That change could not be saved. Try again in a moment.",
    )
    .await
}

/// A cover refusal rendered on the settings page, because a browser form cannot
/// read the plain-text body the JSON route returns.
pub(crate) async fn cover_failure_page(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    refresh_with_error(state, identity, timeline_id, status, message).await
}

/// The settings document. Every interpolated value is escaped here; the page is
/// `noindex` and carries the shared site header like every other form page.
fn settings_html(
    timeline: &Timeline,
    collections: &[Collection],
    flash: Option<&str>,
    error: Option<&str>,
) -> String {
    let id = escape_html(&timeline.id);
    let title = escape_html(&timeline.title);
    let description = escape_html(&timeline.description);
    let action = format!("/api/timeline/{id}/settings");

    let flash = flash
        .map(|message| {
            format!(
                "<p class=\"settings__flash\" role=\"status\">{}</p>",
                escape_html(message)
            )
        })
        .unwrap_or_default();
    let error = error
        .map(|message| {
            format!(
                "<p class=\"form-error-summary\" role=\"alert\">{}</p>",
                escape_html(message)
            )
        })
        .unwrap_or_default();

    let options = collections
        .iter()
        .map(|collection| {
            let selected = timeline.collection_id.as_deref() == Some(collection.id.as_str());
            format!(
                "<option value=\"{}\"{}>{}</option>",
                escape_html(&collection.id),
                if selected { " selected" } else { "" },
                escape_html(&collection.title),
            )
        })
        .collect::<String>();
    let none_selected = if timeline.collection_id.is_none() {
        " selected"
    } else {
        ""
    };

    let cover = match timeline.cover_image.as_ref() {
        Some(cover) => format!(
            "<figure class=\"settings__cover\">\
             <img class=\"settings__cover-image\" src=\"/media/{key}\" width=\"{width}\" height=\"{height}\" alt=\"\" />\
             <figcaption>{credit}</figcaption></figure>",
            key = escape_html(&cover.key),
            width = cover.width,
            height = cover.height,
            credit = escape_html(cover.credit.as_deref().unwrap_or("")),
        ),
        None => "<p class=\"settings__cover-empty\">No cover image: the Timeline shows its own generated gradient.</p>"
            .to_owned(),
    };
    let remove_cover = timeline.cover_image.as_ref().map(|_| {
        format!(
            "<form class=\"settings__inline\" method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"action\" value=\"remove-cover\" />\
             <button type=\"submit\" class=\"settings__danger\">Remove the cover</button></form>"
        )
    });

    let visibility = if timeline.published {
        let confirm = format!(
            "Unpublish \"{}\"? The page and its list entries disappear until you publish it again.",
            timeline.title
        );
        format!(
            "<p>This Timeline is published: readers can find it and open its page.</p>\
             <form method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"action\" value=\"unpublish\" />\
             <button type=\"submit\" data-confirm=\"{confirm}\">Unpublish this Timeline</button></form>",
            confirm = escape_html(&confirm),
        )
    } else {
        format!(
            "<p>This Timeline is unpublished: its page and list entries are hidden. Its content is kept.</p>\
             <form method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"action\" value=\"publish\" />\
             <button type=\"submit\">Publish this Timeline again</button></form>"
        )
    };
    let delete_confirm = escape_html(&format!(
        "Delete \"{}\" permanently? Its Events and suggestions are deleted too. This cannot be undone.",
        timeline.title
    ));

    let page = format!(
        r#"<main class="settings">
  <p class="eyebrow">Owner</p>
  <h1>Timeline settings</h1>
  <p class="lede">Change the details, manage the cover, and control whether this Timeline is live. Every change re-renders the Timeline and the lists it appears in.</p>
  <p class="settings__back"><a href="/timeline/{id}">Back to the Timeline</a></p>
  {flash}
  {error}

  <section class="settings__section" aria-labelledby="settings-details">
    <h2 class="settings__title" id="settings-details">Details</h2>
    <form method="post" action="{action}">
      <input type="hidden" name="action" value="save" />
      <label for="settings-title">Title</label>
      <input id="settings-title" name="title" required value="{title}" />
      <label for="settings-description">Description</label>
      <textarea id="settings-description" name="description">{description}</textarea>
      <label for="settings-collection">Collection</label>
      <select id="settings-collection" name="collectionId">
        <option value=""{none_selected}>No collection</option>
        {options}
      </select>
      <button type="submit">Save changes</button>
    </form>
  </section>

  <section class="settings__section" aria-labelledby="settings-cover">
    <h2 class="settings__title" id="settings-cover">Cover image</h2>
    {cover}
    <form method="post" action="/api/timeline/{id}/cover" enctype="multipart/form-data">
      <label for="settings-cover-file">Upload or replace the image</label>
      <input id="settings-cover-file" type="file" name="file" accept="image/jpeg,image/png,image/webp" required />
      <label for="settings-cover-credit">Credit (optional)</label>
      <input id="settings-cover-credit" name="credit" />
      <p class="help">JPEG, PNG, or WebP, up to 4 MiB. The hero shows the image full bleed.</p>
      <button type="submit">Upload cover</button>
    </form>
    {remove_cover}
  </section>

  <section class="settings__section" aria-labelledby="settings-visibility">
    <h2 class="settings__title" id="settings-visibility">Visibility</h2>
    {visibility}
  </section>

  <section class="settings__section settings__section--danger" aria-labelledby="settings-delete">
    <h2 class="settings__title" id="settings-delete">Delete</h2>
    <p>Deleting removes the Timeline, its Events, its suggestions, and its page. This cannot be undone.</p>
    <form method="post" action="{action}">
      <input type="hidden" name="action" value="delete" />
      <button type="submit" class="settings__danger" data-confirm="{delete_confirm}">Delete this Timeline</button>
    </form>
  </section>
</main>"#,
        remove_cover = remove_cover.unwrap_or_default(),
        none_selected = none_selected,
        options = options,
        visibility = visibility,
    );

    PageDocument::new("Timeline settings", &page)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .stylesheet(SETTINGS_CSS)
        .noindex()
        .render()
}

const SETTINGS_CSS: &str = r#"
.settings { max-width: 46rem; }
.settings__back { margin: 0.9rem 0 1.4rem; }
.settings__back a { color: var(--muted); font-size: 0.9rem; }
.settings__flash {
  margin: 0 0 1.2rem;
  padding: 0.7rem 0.9rem;
  border-left: 3px solid #2f6b46;
  background: #f2f8f4;
  font-size: 0.92rem;
}
.settings__section {
  margin: 1.6rem 0 0;
  padding: 1.2rem 1.35rem;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: #fff;
}
.settings__section--danger { border-color: #e6c9c2; }
.settings__title {
  margin: 0 0 0.5rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--accent);
}
.settings label { display: block; margin: 0.8rem 0 0.3rem; font-weight: 600; font-size: 0.88rem; }
.settings input, .settings textarea, .settings select {
  width: 100%;
  padding: 0.55rem 0.65rem;
  border: 1px solid var(--line);
  border-radius: 5px;
  background: #fff;
  font: inherit;
}
.settings textarea { min-height: 5rem; }
.settings button { margin-top: 0.9rem; }
.settings__cover { margin: 0.4rem 0 0.2rem; }
.settings__cover-image {
  display: block;
  width: 100%;
  max-width: 24rem;
  height: auto;
  border-radius: 5px;
  border: 1px solid var(--line);
}
.settings__cover figcaption { margin-top: 0.35rem; color: var(--muted); font-size: 0.82rem; }
.settings__cover-empty { margin: 0.2rem 0 0.6rem; color: var(--muted); font-style: italic; }
.settings__inline { margin-top: 0.4rem; }
.settings__danger {
  padding: 0.55rem 0.9rem;
  border: 1px solid #9a3220;
  border-radius: 5px;
  background: #fff;
  color: #9a3220;
  font: inherit;
  font-size: 0.9rem;
  cursor: pointer;
}
.settings__danger:hover { background: #fdf1ee; }
.settings__danger:focus-visible, .settings button:focus-visible {
  outline: 2px solid currentColor;
  outline-offset: 2px;
}
"#;

fn flash_for(saved: Option<&str>) -> Option<&'static str> {
    match saved {
        Some("details") => Some("Saved. The Timeline and every list it appears in are updated."),
        Some("cover") => Some("Cover updated. The hero uses the new image."),
        Some("cover-removed") => {
            Some("Cover removed. The Timeline uses its generated gradient again.")
        }
        Some("unpublished") => {
            Some("Unpublished. The page and its list entries are gone until you publish it again.")
        }
        Some("published") => Some("Published. The page is live again."),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Loading and refusals
// ---------------------------------------------------------------------------

/// The Timeline, if it exists and the caller owns it.
async fn load_owned(
    state: &AppState,
    identity: &Identity,
    timeline_id: &str,
) -> Result<Timeline, ManageError> {
    let ddb = state.dynamodb().map_err(ManageError::Config)?;
    let timeline = renderer::dynamo::load_timeline(ddb, &state.config().table_name, timeline_id)
        .await
        .map_err(ManageError::read)?;
    if timeline.owner_id != identity.account_id {
        return Err(ManageError::Forbidden);
    }
    Ok(timeline)
}

#[derive(Debug, Default)]
struct SettingsForm {
    action: String,
    title: String,
    description: String,
    collection_id: Option<String>,
}

async fn read_form(request: Request) -> Result<SettingsForm, String> {
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|error| format!("could not read the form: {error}"))?;
    Ok(parse_form(&bytes))
}

fn parse_form(bytes: &[u8]) -> SettingsForm {
    let mut form = SettingsForm::default();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        let value = value.into_owned();
        match key.as_ref() {
            "action" => form.action = value,
            "title" => form.title = value,
            "description" => form.description = value,
            "collectionId" | "collection_id" => form.collection_id = Some(value),
            _ => {}
        }
    }
    form
}

fn render_notice(error: RenderError) -> Response {
    match error {
        RenderError::Config(error) => ManageError::Config(error).into_notice(),
        RenderError::Internal(message) => ManageError::Internal(message).into_notice(),
    }
}

#[derive(Debug)]
enum ManageError {
    NotFound,
    Forbidden,
    Config(ConfigError),
    Internal(String),
}

impl ManageError {
    fn read(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Self::NotFound;
        }
        Self::Internal(format!("timeline read failed: {error}"))
    }

    /// A refusal the owner can read: a page, not a one-line body.
    fn into_notice(self) -> Response {
        let (status, heading, message) = match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "Timeline not found",
                "This Timeline does not exist any more.".to_owned(),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "Not your Timeline",
                "Only the Timeline's owner can change its settings.".to_owned(),
            ),
            Self::Config(error) => {
                tracing::error!(%error, "settings refused: configuration incomplete");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Settings are unavailable",
                    "Try again in a moment.".to_owned(),
                )
            }
            Self::Internal(message) => {
                tracing::error!(%message, "settings failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "That page cannot be shown right now. Try again.".to_owned(),
                )
            }
        };
        let page = format!(
            "<main><h1>{}</h1><p>{}</p><p><a href=\"/\">Back to Home</a></p></main>",
            escape_html(heading),
            escape_html(&message),
        );
        html_response(
            status,
            PageDocument::new(heading, &page)
                .stylesheet(NOTICE_CSS)
                .noindex()
                .render(),
        )
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

mod store {
    use super::*;
    use data::keys;

    /// Save the title and description, stamping `updatedAt`, and nothing else.
    pub(super) async fn update_details(
        state: &AppState,
        identity: &Identity,
        timeline_id: &str,
        title: &str,
        description: &str,
        now: &Timestamp,
    ) -> Result<(), String> {
        let ddb = state.dynamodb().map_err(|error| format!("{error}"))?;
        ddb.update_item()
            .table_name(&state.config().table_name)
            .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
            .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
            .condition_expression("attribute_exists(PK) AND ownerId = :owner")
            .update_expression("SET title = :title, description = :description, updatedAt = :now")
            .expression_attribute_values(":title", AttributeValue::S(title.to_owned()))
            .expression_attribute_values(":description", AttributeValue::S(description.to_owned()))
            .expression_attribute_values(":now", AttributeValue::S(now.as_str().to_owned()))
            .expression_attribute_values(":owner", AttributeValue::S(identity.account_id.clone()))
            .send()
            .await
            .map_err(|error| format!("UpdateItem(TIMELINE details): {error:?}"))?;
        Ok(())
    }

    /// Flip `published` without touching anything else.
    pub(super) async fn set_published(
        state: &AppState,
        identity: &Identity,
        timeline_id: &str,
        published: bool,
        now: &Timestamp,
    ) -> Result<(), String> {
        let ddb = state.dynamodb().map_err(|error| format!("{error}"))?;
        ddb.update_item()
            .table_name(&state.config().table_name)
            .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
            .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
            .condition_expression("attribute_exists(PK) AND ownerId = :owner")
            .update_expression("SET published = :published, updatedAt = :now")
            .expression_attribute_values(":published", AttributeValue::Bool(published))
            .expression_attribute_values(":now", AttributeValue::S(now.as_str().to_owned()))
            .expression_attribute_values(":owner", AttributeValue::S(identity.account_id.clone()))
            .send()
            .await
            .map_err(|error| format!("UpdateItem(TIMELINE published): {error:?}"))?;
        Ok(())
    }

    /// Delete the Timeline row, its Events, and its Suggestions. REPORTS and
    /// VOTEs live in their own partitions and are unreachable without the
    /// target, so they are left to age out rather than scanned per delete.
    pub(super) async fn delete_contents(
        state: &AppState,
        identity: &Identity,
        timeline_id: &str,
    ) -> Result<(), String> {
        let ddb = state.dynamodb().map_err(|error| format!("{error}"))?;
        let table = &state.config().table_name;

        // Ownership is checked twice on purpose: before deleting anything and
        // in the delete condition for the TIMELINE row itself.
        let timeline = renderer::dynamo::load_timeline(ddb, table, timeline_id)
            .await
            .map_err(|error| format!("timeline read failed: {error}"))?;
        if timeline.owner_id != identity.account_id {
            return Err("not the owner".to_owned());
        }

        let mut keys_to_delete: Vec<(String, String)> = Vec::new();
        keys_to_delete.push((keys::timeline_pk(timeline_id), keys::TIMELINE_SK.to_owned()));
        for partition in [
            keys::timeline_pk(timeline_id),
            keys::suggestion_pk(timeline_id),
        ] {
            let mut start_key = None;
            loop {
                let mut query = ddb
                    .query()
                    .table_name(table)
                    .key_condition_expression("PK = :pk")
                    .expression_attribute_values(":pk", AttributeValue::S(partition.clone()));
                if let Some(key) = start_key.take() {
                    query = query.set_exclusive_start_key(Some(key));
                }
                let response = query
                    .send()
                    .await
                    .map_err(|error| format!("Query(delete): {error:?}"))?;
                for item in response.items.unwrap_or_default() {
                    let (Some(AttributeValue::S(pk)), Some(AttributeValue::S(sk))) =
                        (item.get("PK"), item.get("SK"))
                    else {
                        continue;
                    };
                    let pair = (pk.clone(), sk.clone());
                    if !keys_to_delete.contains(&pair) {
                        keys_to_delete.push(pair);
                    }
                }
                match response.last_evaluated_key {
                    Some(key) if !key.is_empty() => start_key = Some(key),
                    _ => break,
                }
            }
        }

        for chunk in keys_to_delete.chunks(25) {
            let writes = chunk
                .iter()
                .map(|(pk, sk)| {
                    WriteRequest::builder()
                        .delete_request(
                            DeleteRequest::builder()
                                .key("PK", AttributeValue::S(pk.clone()))
                                .key("SK", AttributeValue::S(sk.clone()))
                                .build()
                                .expect("delete request"),
                        )
                        .build()
                })
                .collect::<Vec<_>>();
            ddb.batch_write_item()
                .request_items(table, writes)
                .send()
                .await
                .map_err(|error| format!("BatchWriteItem(delete): {error:?}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_form_reads_every_field_the_page_posts() {
        let form = parse_form(b"action=save&title=New+title&description=New+deck&collectionId=c1");
        assert_eq!(form.action, "save");
        assert_eq!(form.title, "New title");
        assert_eq!(form.description, "New deck");
        assert_eq!(form.collection_id.as_deref(), Some("c1"));

        let unpublish = parse_form(b"action=unpublish");
        assert_eq!(unpublish.action, "unpublish");
        assert_eq!(unpublish.collection_id, None);
    }

    #[test]
    fn every_saved_outcome_has_a_sentence() {
        for saved in [
            "details",
            "cover",
            "cover-removed",
            "unpublished",
            "published",
        ] {
            assert!(
                flash_for(Some(saved)).is_some(),
                "saved={saved} needs a flash"
            );
        }
        assert!(flash_for(Some("nonsense")).is_none());
        assert!(flash_for(None).is_none());
    }

    #[test]
    fn the_page_is_one_document_with_the_shared_header_and_no_index() {
        let timeline = sample_timeline(true);
        let html = settings_html(&timeline, &[], None, None);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<header class=\"site-header\">"));
        assert!(html.contains("noindex"));
        assert_eq!(html.matches("<style").count(), 1);
        assert!(html.contains("Timeline settings"));
        assert!(html.contains("Back to the Timeline"));
    }

    #[test]
    fn a_published_timeline_offers_unpublish_and_delete_behind_confirmations() {
        let html = settings_html(&sample_timeline(true), &[], None, None);
        assert!(html.contains("name=\"action\" value=\"unpublish\""));
        assert!(html.contains("Unpublish this Timeline"));
        assert!(html.contains("name=\"action\" value=\"delete\""));
        assert!(
            html.contains("data-confirm=\"Delete &quot;Ohio Senate 2026&quot;"),
            "delete asks first: {html}"
        );
        assert!(!html.contains("value=\"publish\""));
    }

    #[test]
    fn an_unpublished_timeline_says_its_content_is_kept_and_offers_publishing() {
        let html = settings_html(&sample_timeline(false), &[], None, None);
        assert!(html.contains("name=\"action\" value=\"publish\""));
        assert!(html.contains("Publish this Timeline again"));
        assert!(html.contains("content is kept"));
        assert!(!html.contains("value=\"unpublish\""));
    }

    #[test]
    fn the_cover_section_previews_what_is_there_and_offers_removal() {
        let mut timeline = sample_timeline(true);
        timeline.cover_image = Some(data::model::CoverImage {
            key: "cover/t1/abc.jpg".to_owned(),
            width: 1600,
            height: 900,
            credit: Some("Photo: Jane".to_owned()),
        });
        let html = settings_html(&timeline, &[], None, None);
        assert!(html.contains("src=\"/media/cover/t1/abc.jpg\""));
        assert!(html.contains("Photo: Jane"));
        assert!(html.contains("value=\"remove-cover\""));
        assert!(html.contains("JPEG, PNG, or WebP, up to 4 MiB"));

        let none = settings_html(&sample_timeline(true), &[], None, None);
        assert!(none.contains("No cover image"));
        assert!(!none.contains("value=\"remove-cover\""));
    }

    #[test]
    fn the_collection_picker_marks_the_current_collection() {
        let mut timeline = sample_timeline(true);
        timeline.collection_id = Some("c1".to_owned());
        let collections = [data::model::Collection {
            id: "c1".to_owned(),
            title: "Elections".to_owned(),
            description: String::new(),
            parent_collection_id: None,
            created_at: Timestamp::now(),
            created_by: "owner".to_owned(),
        }];
        let html = settings_html(&timeline, &collections, None, None);
        assert!(html.contains("<option value=\"c1\" selected>Elections</option>"));
        assert!(html.contains("<option value=\"\">No collection</option>"));
    }

    fn sample_timeline(published: bool) -> Timeline {
        Timeline {
            id: "t1".to_owned(),
            title: "Ohio Senate 2026".to_owned(),
            description: "The race.".to_owned(),
            owner_id: "owner".to_owned(),
            collection_id: None,
            event_count: 1,
            upvotes: 0,
            downvotes: 0,
            report_count: 0,
            flagged: false,
            published,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            cover_image: None,
        }
    }
}
