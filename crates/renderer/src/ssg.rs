//! Static-site generation: route list, per-route render, and `static_file_path`
//! writes.
//!
//! Leptos's built-in [`leptos_axum::StaticRouteGenerator`] uses its streaming
//! response path, which appends two `<script>` tags for hydration resource
//! bookkeeping. Read pages ship no hydration, so this renderer keeps the route
//! list (Leptos still needs it to enumerate static routes) but renders each
//! path through the hand-rolled `render_app_async_with_context` handler, which
//! emits a plain document, and then writes it with `static_file_path`.
//!
//! The executor requirement this path carries is documented in
//! [`crate::executor`].

use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use leptos::config::LeptosOptions;
use leptos::prelude::*;
use leptos_integration_utils::static_file_path;
use leptos_router::SsrMode;
use leptos_router::static_routes::StaticParamsMap;

use crate::page::Site;
use crate::ui;

/// One file written by a render run.
#[derive(Clone, Debug)]
pub struct GeneratedPage {
    /// Viewer path, e.g. `/timeline/ohio-senate-2026`.
    pub route_path: String,
    /// On-disk path, e.g. `<out>/timeline/ohio-senate-2026.html`.
    pub file_path: PathBuf,
    /// Size of the written document in bytes.
    pub bytes: usize,
}

/// Render every static route in `site` under `options.site_root`.
///
/// Call [`crate::init_executor`] first; the route-list walk and the render both
/// need the SSG executor installed.
pub async fn generate_site(
    site: &Site,
    options: &LeptosOptions,
) -> Result<Vec<GeneratedPage>, Box<dyn Error + Send + Sync>> {
    let route_site = site.clone();
    let context_site = site.clone();
    let app_fn = move || ui::app(route_site.clone());

    // Route-list generation: this is the getStaticPaths analogue. It discovers
    // the declared static routes and carries the `prerender_params` closures
    // that list every Timeline id.
    let (routes, _generator) = leptos_axum::generate_route_list_with_exclusions_and_ssg_and_context(
        app_fn,
        None,
        move || provide_context(context_site.clone()),
    );

    let mut generated = Vec::new();
    for route in &routes {
        let SsrMode::Static(static_route) = route.mode() else {
            continue;
        };
        let Some(params) = static_route.to_prerendered_params().await else {
            continue;
        };

        for assignment in expand_params(&params) {
            let route_path = fill_path(route.path(), &assignment)?;
            let html = render_route(site, &route_path).await?;
            let html = strip_hydration_bookkeeping(html);
            let file_path = PathBuf::from(static_file_path(options, &route_path));

            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&file_path, html.as_bytes()).await?;
            generated.push(GeneratedPage {
                route_path,
                file_path,
                bytes: html.len(),
            });
        }
    }

    Ok(generated)
}

/// Render one path through the router to a complete HTML string.
async fn render_route(site: &Site, path: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let context_site = site.clone();
    let app_site = site.clone();
    let handler = leptos_axum::render_app_async_with_context(
        move || provide_context(context_site.clone()),
        move || ui::app(app_site.clone()),
    );

    let request = Request::builder()
        .method("GET")
        .uri(path)
        .header(header::ACCEPT, "text/html")
        .body(Body::empty())?;

    let response = handler(request).await;
    if response.status() != StatusCode::OK {
        return Err(io::Error::other(format!(
            "rendering {path} returned HTTP {}",
            response.status()
        ))
        .into());
    }

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8(bytes.to_vec())?)
}

/// Cross-product of every param's candidate values. A static route with one
/// `:id` param becomes one assignment per Timeline id.
fn expand_params(params: &StaticParamsMap) -> Vec<HashMap<String, String>> {
    let mut assignments = vec![HashMap::new()];
    for (name, values) in &params.0 {
        let mut next = Vec::with_capacity(assignments.len() * values.len());
        for base in &assignments {
            for value in values {
                let mut assignment = base.clone();
                assignment.insert(name.clone(), value.clone());
                next.push(assignment);
            }
        }
        assignments = next;
    }
    assignments
}

/// Replace `:name` segments with their values. Refuses path characters that
/// could escape the output directory, so a hostile Timeline id cannot write
/// outside `site_root`.
fn fill_path(
    pattern: &str,
    params: &HashMap<String, String>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut segments = Vec::new();
    for segment in pattern.split('/') {
        match param_name(segment) {
            Some(name) => {
                let value = params.get(name).ok_or_else(|| {
                    io::Error::other(format!(
                        "static route {pattern:?} has no prerendered value for {name:?}"
                    ))
                })?;
                if !is_safe_path_segment(value) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "timeline id {value:?} is not a safe path segment; \
                             ids must be ASCII letters, digits, '-' or '_'"
                        ),
                    )
                    .into());
                }
                segments.push(value.as_str());
            }
            None => segments.push(segment),
        }
    }
    Ok(segments.join("/"))
}

/// Axum-flavored route listings use `{id}`, while Leptos route definitions use
/// `:id`; accept both so route resolution is not tied to the listing format.
fn param_name(segment: &str) -> Option<&str> {
    if let Some(name) = segment.strip_prefix(':') {
        return Some(name);
    }
    segment
        .strip_prefix('{')
        .and_then(|name| name.strip_suffix('}'))
}

fn is_safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        && value != "."
        && value != ".."
}

/// Drop the SSR resource bookkeeping scripts Leptos appends for hydration.
///
/// The read page ships no hydration bundle and no scripts at all. These two
/// tags only initialize globals a hydration client would consume, so removing
/// them leaves the document standalone. Any other script tag is preserved.
fn strip_hydration_bookkeeping(html: String) -> String {
    let mut output = String::with_capacity(html.len());
    let mut rest = html.as_str();

    while let Some(start) = rest.find("<script") {
        let Some(relative_end) = rest[start..].find("</script>") else {
            break;
        };
        let end = start + relative_end + "</script>".len();
        let tag = &rest[start..end];
        output.push_str(&rest[..start]);
        if !(tag.contains("__RESOLVED_RESOURCES") || tag.contains("__INCOMPLETE_CHUNKS")) {
            output.push_str(tag);
        }
        rest = &rest[end..];
    }

    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_path_substitutes_a_safe_segment() {
        let mut params = HashMap::new();
        params.insert("id".to_owned(), "ohio-senate-2026".to_owned());
        assert_eq!(
            fill_path("/timeline/:id", &params).expect("path fills"),
            "/timeline/ohio-senate-2026"
        );
        assert_eq!(
            fill_path("/timeline/{id}", &params).expect("axum-style path fills"),
            "/timeline/ohio-senate-2026"
        );
    }

    #[test]
    fn fill_path_refuses_traversal() {
        let mut params = HashMap::new();
        params.insert("id".to_owned(), "../../etc/passwd".to_owned());
        assert!(fill_path("/timeline/:id", &params).is_err());
    }

    #[test]
    fn expand_params_builds_the_cross_product() {
        let mut params = StaticParamsMap::new();
        params.insert("id", vec!["a".to_owned(), "b".to_owned()]);
        let assignments = expand_params(&params);
        assert_eq!(assignments.len(), 2);
    }

    #[test]
    fn strip_removes_only_bookkeeping_scripts() {
        let html = concat!(
            "<!DOCTYPE html><html><body>hi</body></html>",
            "<script nonce=\"x\">__RESOLVED_RESOURCES=[];</script>",
            "<script nonce=\"x\">__INCOMPLETE_CHUNKS=[];</script>",
        );
        let stripped = strip_hydration_bookkeeping(html.to_owned());
        assert!(!stripped.contains("<script"));
        assert!(stripped.contains("<body>hi</body>"));
    }
}
