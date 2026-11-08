//! Shared re-render + invalidation step for every Timeline mutation.
//!
//! Publish (ticket 09) and cover upload/remove both dirty the same page set:
//! the Timeline page, its collection page, and Home (PRD §4). They also share
//! the exact-path CloudFront invalidation, which is best-effort so a cache
//! failure never turns a committed write into a 5xx.

use data::model::Timeline;

use crate::AppState;
use crate::config::ConfigError;

/// Render-failure shape, mapped by each caller's own error type.
#[derive(Debug)]
pub(crate) enum RenderError {
    Config(ConfigError),
    Internal(String),
}

/// Re-render the affected viewer paths, upload them to the site bucket, and
/// invalidate both the viewer and rewritten-object URIs.
pub(crate) async fn rerender(state: &AppState, timeline: &Timeline) -> Result<(), RenderError> {
    renderer::init_executor();

    let ddb = state.dynamodb().map_err(RenderError::Config)?;
    let site = renderer::dynamo::load_site(ddb, &state.config().table_name, None)
        .await
        .map_err(|error| RenderError::Internal(format!("renderer read failed: {error}")))?;
    let s3 = state.s3().map_err(RenderError::Config)?;

    let paths = affected_paths(timeline);
    let published = renderer::publish_routes(s3, &state.config().site_bucket, &site, &paths)
        .await
        .map_err(|error| RenderError::Internal(format!("render/upload failed: {error}")))?;

    tracing::info!(
        timeline_id = %timeline.id,
        pages = published.len(),
        "rendered and uploaded the affected page set"
    );

    invalidate(state, &timeline.id, &invalidation_paths(timeline)).await;

    Ok(())
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

/// Pages the renderer must re-render when a Timeline changes (PRD §4).
pub(crate) fn affected_paths(timeline: &Timeline) -> Vec<String> {
    let mut paths = vec![format!("/timeline/{}", timeline.id)];
    if let Some(collection_id) = &timeline.collection_id
        && is_safe_segment(collection_id)
    {
        paths.push(format!("/collection/{collection_id}"));
    }
    paths.push("/".to_owned());
    paths
}

/// Exact invalidation paths: the viewer URI and the object URI are distinct
/// cache keys because the viewer-request rewrite appends `.html` (PRD §4).
pub(crate) fn invalidation_paths(timeline: &Timeline) -> Vec<String> {
    let mut paths = Vec::new();
    let timeline_path = format!("/timeline/{}", timeline.id);
    paths.push(timeline_path.clone());
    paths.push(format!("{timeline_path}.html"));

    if let Some(collection_id) = &timeline.collection_id
        && is_safe_segment(collection_id)
    {
        let collection_path = format!("/collection/{collection_id}");
        paths.push(collection_path.clone());
        paths.push(format!("{collection_path}.html"));
    }

    paths.push("/".to_owned());
    paths.push("/index.html".to_owned());
    paths
}

/// Best-effort exact-path CloudFront invalidation. Locally there is no
/// distribution: a missing value, a client-less state, or the seed script's
/// `local-distribution` placeholder logs and returns. In production an SDK
/// failure is logged rather than failing a write that already committed.
async fn invalidate(state: &AppState, timeline_id: &str, paths: &[String]) {
    let Some(client) = state.cloudfront_opt() else {
        tracing::info!("CloudFront invalidation skipped: no client is configured");
        return;
    };
    let distribution_id = match state.runtime_config().await {
        Ok(runtime) => runtime.distribution_id.clone(),
        Err(error) => {
            tracing::info!(%error, "CloudFront invalidation skipped: distribution id unavailable");
            return;
        }
    };
    if !is_real_distribution_id(&distribution_id) {
        tracing::info!(
            distribution_id = %distribution_id,
            paths = ?paths,
            "CloudFront invalidation skipped: local/placeholder distribution id"
        );
        return;
    }

    let caller_reference = format!(
        "mutation-{timeline_id}-{}",
        chrono::Utc::now().timestamp_millis()
    );
    let batch = match cloudfront_batch(caller_reference, paths) {
        Ok(batch) => batch,
        Err(message) => {
            tracing::warn!(%message, "CloudFront invalidation skipped: invalid batch");
            return;
        }
    };

    match client
        .create_invalidation()
        .distribution_id(&distribution_id)
        .invalidation_batch(batch)
        .send()
        .await
    {
        Ok(output) => tracing::info!(
            invalidation_id = output
                .invalidation()
                .map(|invalidation| invalidation.id())
                .unwrap_or("unknown"),
            paths = ?paths,
            "invalidated the affected paths"
        ),
        Err(error) => {
            tracing::warn!(%error, "CloudFront invalidation failed; write already committed")
        }
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

pub(crate) fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
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
    fn affected_paths_are_timeline_collection_and_home() {
        let timeline = timeline(Some("c1"));
        assert_eq!(
            affected_paths(&timeline),
            ["/timeline/t1", "/collection/c1", "/"]
        );
        assert_eq!(
            invalidation_paths(&timeline),
            [
                "/timeline/t1",
                "/timeline/t1.html",
                "/collection/c1",
                "/collection/c1.html",
                "/",
                "/index.html"
            ]
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
