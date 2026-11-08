//! The synchronous render → S3 `PUT` → CloudFront invalidation step shared by
//! every mutation that dirties a static page (PRD §4 and §5).
//!
//! Ticket 09 built this path for publishing; Collections reuse it unchanged so
//! a membership change and a publish can never drift apart in behaviour.

use crate::AppState;

/// Re-render `page_paths` from the current table state, upload each document to
/// the site bucket, then invalidate the exact viewer and object URIs.
pub async fn rerender(state: &AppState, page_paths: &[String]) -> Result<(), String> {
    renderer::init_executor();

    let ddb = state.dynamodb().map_err(|error| error.to_string())?;
    let site = renderer::dynamo::load_site(ddb, &state.config().table_name, None)
        .await
        .map_err(|error| format!("renderer read failed: {error}"))?;
    let s3 = state.s3().map_err(|error| error.to_string())?;

    let published = renderer::publish_routes(s3, &state.config().site_bucket, &site, page_paths)
        .await
        .map_err(|error| format!("render/upload failed: {error}"))?;

    tracing::info!(
        pages = published.len(),
        "rendered and uploaded the affected page set"
    );

    invalidate(state, &invalidation_paths(page_paths)).await;
    Ok(())
}

/// Remove a page object that no longer has a document (a deleted Collection).
/// Best-effort: the row is already gone, so a failure is logged, not returned.
pub async fn delete_page(state: &AppState, page_path: &str) {
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
        Ok(_) => invalidate(state, &invalidation_paths(&[page_path.to_owned()])).await,
        Err(error) => tracing::warn!(%error, key, "could not delete the stale page object"),
    }
}

/// Exact invalidation keys: the viewer URI and the object URI are distinct
/// cache keys because the viewer-request rewrite appends `.html` (PRD §4).
pub fn invalidation_paths(page_paths: &[String]) -> Vec<String> {
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

/// Best-effort exact-path CloudFront invalidation. Locally there is no
/// distribution: a missing value, a client-less state, or the seed script's
/// `local-distribution` placeholder logs and returns. In production an SDK
/// failure is logged rather than failing a write that already committed.
async fn invalidate(state: &AppState, paths: &[String]) {
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

    let caller_reference = format!("render-{}", chrono::Utc::now().timestamp_millis());
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn placeholder_distribution_ids_skip_invalidation() {
        assert!(!is_real_distribution_id(""));
        assert!(!is_real_distribution_id("local-distribution"));
        assert!(!is_real_distribution_id("http://localhost:4566/cloudfront"));
        assert!(is_real_distribution_id("E2ABCDEFGHIJKL"));
    }
}
