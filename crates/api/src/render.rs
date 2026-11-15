//! The synchronous render → S3 `PUT` → CloudFront invalidation step shared by
//! every mutation that dirties a static page (PRD §4 and §5).
//!
//! Publish, cover upload/remove, suggestion review, and collection changes all
//! dirty the same page set and share this path, so their render behaviour can
//! never drift apart. Publish and cover re-render a single Timeline; collection
//! membership changes re-render an explicit set of pages via [`rerender_paths`].

use data::model::Timeline;

use crate::AppState;
use crate::config::ConfigError;

/// Render-failure shape, mapped by each caller's own error type.
#[derive(Debug)]
pub(crate) enum RenderError {
    Config(ConfigError),
    Internal(String),
}

/// Re-render the page set a Timeline mutation dirtied: the Timeline page, its
/// collection page (if collected), and Home (PRD §4).
pub(crate) async fn rerender(state: &AppState, timeline: &Timeline) -> Result<(), RenderError> {
    rerender_paths(state, &affected_paths(timeline)).await
}

/// Re-render an explicit set of viewer paths from the current table state,
/// upload each document to the site bucket, then invalidate the exact viewer
/// and object URIs.
///
/// [`rerender`] delegates here; Collections call this directly because a
/// membership change re-renders several pages that are not one Timeline.
pub(crate) async fn rerender_paths(
    state: &AppState,
    page_paths: &[String],
) -> Result<(), RenderError> {
    renderer::init_executor();

    let ddb = state.dynamodb().map_err(RenderError::Config)?;
    // Home re-renders on every mutation (PRD §4) and sorts all Timelines by
    // recency, and a collection page needs every child and its direct
    // Timelines. The schema deliberately has no all-timelines index (PRD §6),
    // so a filtered scan is the correct read for the home/collection subset —
    // this is the same read the full-site render uses.
    let site = renderer::dynamo::load_site(ddb, &state.config().table_name, None)
        .await
        .map_err(|error| RenderError::Internal(format!("renderer read failed: {error}")))?;
    let s3 = state.s3().map_err(RenderError::Config)?;

    let published = renderer::publish_routes(s3, &state.config().site_bucket, &site, page_paths)
        .await
        .map_err(|error| RenderError::Internal(format!("render/upload failed: {error}")))?;

    tracing::info!(
        pages = published.len(),
        "rendered and uploaded the affected page set"
    );

    invalidate(state, &invalidation_paths(page_paths)).await?;
    Ok(())
}

/// Remove a page object that no longer has a document (a deleted Collection).
/// Best-effort: the row is already gone, so a failure is logged, not returned.
pub(crate) async fn delete_page(state: &AppState, page_path: &str) {
    let Ok(s3) = state.s3() else {
        tracing::info!("page delete skipped: no S3 client is configured");
        return;
    };
    let key = renderer::object_key(page_path);
    match s3
        .delete_object()
        .bucket(&state.config().site_bucket)
        .key(&key)
        .send()
        .await
    {
        Ok(_) => {
            if let Err(error) =
                invalidate(state, &invalidation_paths(&[page_path.to_owned()])).await
            {
                tracing::warn!(?error, "page deleted but invalidation failed");
            }
        }
        Err(error) => tracing::warn!(%error, key, "could not delete the stale page object"),
    }
}

/// Render one Timeline page (already re-rendered from DynamoDB) and return the
/// `<header class="hero ...">...</header>` fragment for read-your-writes.
pub(crate) async fn timeline_hero(
    state: &AppState,
    timeline_id: &str,
) -> Result<String, RenderError> {
    renderer::init_executor();

    let ddb = state.dynamodb().map_err(RenderError::Config)?;
    let site = renderer::dynamo::load_site(ddb, &state.config().table_name, Some(timeline_id))
        .await
        .map_err(|error| RenderError::Internal(format!("renderer read failed: {error}")))?;
    let path = format!("/timeline/{timeline_id}");
    let html = renderer::render_path(&site, &path)
        .await
        .map_err(|error| RenderError::Internal(format!("render failed: {error}")))?;

    extract_hero(&html).map(str::to_owned).ok_or_else(|| {
        RenderError::Internal("the rendered page carried no hero fragment".to_owned())
    })
}

/// Slice out the hero header. The renderer emits exactly one `<header>` per
/// Timeline document, but Leptos does not promise attribute order, so match on
/// the tag and then check its `class` for the hero marker.
pub(crate) fn extract_hero(html: &str) -> Option<&str> {
    let mut rest = html;
    let mut offset = 0;
    while let Some(found) = rest.find("<header") {
        let start = offset + found;
        let open_end = html[start..].find('>')? + start;
        let opening_tag = &html[start..=open_end];
        if opening_tag.contains("class=\"hero") {
            let end = start + html[start..].find("</header>")? + "</header>".len();
            return Some(&html[start..end]);
        }
        let next = open_end + 1;
        rest = &html[next..];
        offset = next;
    }
    None
}

/// Viewer paths the renderer must re-render when a Timeline changes (PRD §4).
pub(crate) fn affected_paths(timeline: &Timeline) -> Vec<String> {
    let mut paths = vec![format!("/timeline/{}", timeline.id)];
    if let Some(collection_id) = &timeline.collection_id
        && is_safe_segment(collection_id)
    {
        paths.push(format!("/collection/{collection_id}"));
    }
    // Home's preview and the full list are driven by the same set of Timelines,
    // so a change that reaches one reaches both.
    paths.push("/".to_owned());
    paths.push("/timelines".to_owned());
    paths
}

/// Exact invalidation paths: the viewer URI and the object URI are distinct
/// cache keys because the viewer-request rewrite appends `.html` (PRD §4).
pub(crate) fn invalidation_paths(page_paths: &[String]) -> Vec<String> {
    let mut paths = Vec::with_capacity(page_paths.len() * 2);
    for path in page_paths {
        paths.push(path.clone());
        if path == "/" {
            paths.push("/index.html".to_owned());
        } else {
            paths.push(format!("{path}.html"));
        }
    }
    paths
}

/// A single path segment safe to embed in a URL or S3 key.
pub(crate) fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// Exact-path CloudFront invalidation (PRD §4). Invalidation is synchronous in
/// the write path: a failure against a real distribution is returned, not
/// swallowed. Locally there is no distribution, so a missing client, a missing
/// id, or the seed script's `local-distribution` placeholder is a no-op `Ok`.
async fn invalidate(state: &AppState, paths: &[String]) -> Result<(), RenderError> {
    let Some(client) = state.cloudfront_opt() else {
        tracing::info!("CloudFront invalidation skipped: no client is configured");
        return Ok(());
    };
    let distribution_id = match state.runtime_config().await {
        Ok(runtime) => runtime.distribution_id.clone(),
        Err(error) => {
            tracing::info!(%error, "CloudFront invalidation skipped: distribution id unavailable");
            return Ok(());
        }
    };
    if !is_real_distribution_id(&distribution_id) {
        tracing::info!(
            distribution_id = %distribution_id,
            paths = ?paths,
            "CloudFront invalidation skipped: local/placeholder distribution id"
        );
        return Ok(());
    }

    let caller_reference = format!("render-{}", chrono::Utc::now().timestamp_millis());
    let batch = cloudfront_batch(caller_reference, paths).map_err(|message| {
        RenderError::Internal(format!("invalid invalidation batch: {message}"))
    })?;

    match client
        .create_invalidation()
        .distribution_id(&distribution_id)
        .invalidation_batch(batch)
        .send()
        .await
    {
        Ok(output) => {
            tracing::info!(
                invalidation_id = output
                    .invalidation()
                    .map(|invalidation| invalidation.id())
                    .unwrap_or("unknown"),
                paths = ?paths,
                "invalidated the affected paths"
            );
            Ok(())
        }
        Err(error) => Err(RenderError::Internal(format!(
            "CloudFront invalidation failed: {error:?}"
        ))),
    }
}

fn cloudfront_batch(
    caller_reference: String,
    paths: &[String],
) -> Result<aws_sdk_cloudfront::types::InvalidationBatch, String> {
    use aws_sdk_cloudfront::types::{InvalidationBatch, Paths};

    let paths = Paths::builder()
        .quantity(i32::try_from(paths.len()).map_err(|_| "too many invalidation paths")?)
        .set_items(Some(paths.to_vec()))
        .build()
        .map_err(|error| error.to_string())?;

    InvalidationBatch::builder()
        .caller_reference(caller_reference)
        .paths(paths)
        .build()
        .map_err(|error| error.to_string())
}

fn is_real_distribution_id(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('E')
        && value.len() >= 13
        && value.chars().all(|ch| ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::Timestamp;

    fn timeline(collection: Option<&str>) -> Timeline {
        Timeline {
            id: "t1".to_owned(),
            title: "T".to_owned(),
            description: String::new(),
            owner_id: "a".to_owned(),
            collection_id: collection.map(str::to_owned),
            event_count: 1,
            upvotes: 0,
            downvotes: 0,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            report_count: 0,
            flagged: false,
            cover_image: None,
        }
    }

    #[test]
    fn invalidation_paths_cover_viewer_and_object_uris() {
        let paths = invalidation_paths(&[
            "/".to_owned(),
            "/collection/c1".to_owned(),
            "/timeline/t1".to_owned(),
        ]);
        assert_eq!(
            paths,
            [
                "/",
                "/index.html",
                "/collection/c1",
                "/collection/c1.html",
                "/timeline/t1",
                "/timeline/t1.html"
            ]
        );
    }

    #[test]
    fn affected_paths_are_timeline_collection_home_and_the_full_list() {
        let collected = timeline(Some("c1"));
        assert_eq!(
            affected_paths(&collected),
            ["/timeline/t1", "/collection/c1", "/", "/timelines"]
        );
        assert_eq!(
            invalidation_paths(&affected_paths(&collected)),
            [
                "/timeline/t1",
                "/timeline/t1.html",
                "/collection/c1",
                "/collection/c1.html",
                "/",
                "/index.html",
                "/timelines",
                "/timelines.html"
            ]
        );

        let uncollected = timeline(None);
        assert_eq!(
            affected_paths(&uncollected),
            ["/timeline/t1", "/", "/timelines"],
            "an uncollected Timeline never re-renders a collection page"
        );
    }

    #[test]
    fn placeholder_distribution_ids_skip_invalidation() {
        assert!(!is_real_distribution_id(""));
        assert!(!is_real_distribution_id("local-distribution"));
        assert!(!is_real_distribution_id("http://localhost:4566/cloudfront"));
        assert!(is_real_distribution_id("E2ABCDEFGHIJKL"));
    }

    #[test]
    fn hero_extraction_returns_only_the_header() {
        // Leptos emits attributes in whatever order it likes; `data-gradient`
        // before `class` is what the real renderer produces.
        let html = "<html><body><header class=\"index__hero\">home</header>\
                    <header data-gradient=\"1\" class=\"hero hero--cover\">\
                    <img class=\"hero__cover\" src=\"/media/x.png\"/>\
                    </header><main>body</main></body></html>";
        let hero = extract_hero(html).expect("hero is present");
        assert!(hero.starts_with("<header data-gradient=\"1\""));
        assert!(hero.contains("class=\"hero hero--cover\""));
        assert!(hero.ends_with("</header>"));
        assert!(!hero.contains("<main>"));
        assert!(!hero.contains("index__hero"));
    }
}
