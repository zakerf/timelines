//! Who may edit one Timeline, for the client-side owner chrome.
//!
//! Static pages cannot know the viewer, so owner-only controls ship `hidden`
//! and the site script asks this endpoint before revealing them. It enforces
//! nothing: every write already refuses a non-owner. Its only job is to answer
//! `{ "canEdit": true | false }` without leaking the owner's account id (or
//! anything else about them) to the caller.
//!
//! Anonymous callers answer `false` from the session cookie alone, so the
//! common case needs no table read.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;

use crate::AppState;
use crate::auth::OptionalIdentity;
use crate::http::{json_response, plain};

/// Mount the access probes under `/api`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/timeline/{timeline_id}/access", get(timeline_access))
        .route(
            "/collections/{collection_id}/access",
            get(collection_access),
        )
}

/// `GET /api/collections/{collection_id}/access` — the same one-boolean answer
/// for a Collection's owner controls.
async fn collection_access(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(collection_id): Path<String>,
) -> Response {
    let Some(identity) = identity else {
        return can_edit(false);
    };
    let Some(ddb) = state.dynamodb_opt() else {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "collection access unavailable",
        );
    };
    let collection = match crate::collections::store::get_collection(
        ddb,
        &state.config().table_name,
        &collection_id,
    )
    .await
    {
        Ok(Some(collection)) => collection,
        Ok(None) => return plain(StatusCode::NOT_FOUND, "collection not found"),
        Err(error) => {
            tracing::error!(%error, "collection access read failed");
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "collection access failed",
            );
        }
    };

    can_edit(collection.created_by == identity.account_id)
}

/// `GET /api/timeline/{timeline_id}/access` — `200` for any Timeline that
/// exists; anonymous and non-owner callers both get `canEdit: false`.
async fn timeline_access(
    State(state): State<AppState>,
    OptionalIdentity(identity): OptionalIdentity,
    Path(timeline_id): Path<String>,
) -> Response {
    let Some(identity) = identity else {
        return can_edit(false);
    };

    let Some(ddb) = state.dynamodb_opt() else {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeline access unavailable",
        );
    };
    let timeline = match renderer::dynamo::load_timeline(
        ddb,
        &state.config().table_name,
        &timeline_id,
    )
    .await
    {
        Ok(timeline) => timeline,
        Err(error) if renderer::dynamo::is_not_found(error.as_ref()) => {
            return plain(StatusCode::NOT_FOUND, "timeline not found");
        }
        Err(error) => {
            tracing::error!(%error, "timeline access read failed");
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "timeline access failed");
        }
    };

    // An unpublished Timeline is not on the site, so it answers like a missing
    // one: this probe must not confirm that a hidden row exists.
    if !timeline.published && timeline.owner_id != identity.account_id {
        return plain(StatusCode::NOT_FOUND, "timeline not found");
    }

    // Nothing about the owner leaves this handler except this boolean.
    can_edit(timeline.owner_id == identity.account_id)
}

/// The response shape the chrome reads: one camelCase boolean, `no-store`.
fn can_edit(can_edit: bool) -> Response {
    json_response(StatusCode::OK, &serde_json::json!({ "canEdit": can_edit }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn the_payload_is_one_camel_case_flag() {
        let body = to_bytes(can_edit(true).into_body(), usize::MAX)
            .await
            .expect("body");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("json body is readable");
        assert_eq!(value, serde_json::json!({ "canEdit": true }));

        let body = to_bytes(can_edit(false).into_body(), usize::MAX)
            .await
            .expect("body");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("json body is readable");
        assert_eq!(value, serde_json::json!({ "canEdit": false }));
    }
}
