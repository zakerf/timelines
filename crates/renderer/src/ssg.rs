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

use aws_sdk_s3::primitives::ByteStream;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use leptos::config::LeptosOptions;
use leptos::prelude::*;
use leptos_integration_utils::static_file_path;
use leptos_router::SsrMode;
use leptos_router::static_routes::StaticParamsMap;

use crate::page::Site;
use crate::shell::{NOTICE_CSS, PageDocument};
use crate::ui;

/// CloudFront serves this document for both origin `403` and `404` (PRD §4).
/// It is generated with the rest of the site so `aws s3 sync` uploads it.
/// The shell, tokens, and light theme come from [`crate::shell`].
const NOT_FOUND_BODY: &str = r#"<main>
  <p class="eyebrow">404</p>
  <h1>Page not found</h1>
  <p class="lede">The address may be wrong, or this page may have been unpublished or deleted.</p>
  <p><a href="/">Back to Home</a></p>
</main>"#;

/// The generated `/404.html`: light-themed, `noindex`, script-free.
fn not_found_html() -> String {
    PageDocument::new("Page not found", NOT_FOUND_BODY)
        .noindex()
        .stylesheet(NOTICE_CSS)
        .render()
}

/// Served path of the read-page interactivity script. HTMX is the progressive-
/// enhancement layer read pages ship (PRD §4); the bundle is vendored and
/// emitted beside the pages (see [`generate_site`]) so a render run uploads it
/// with the site.
pub const HTMX_ASSET_PATH: &str = "/assets/htmx.min.js";

/// Served path of the read-page behaviour script. It owns the vote widget's
/// error messages, the signed-out sign-in prompt, and the own-vote paint (see
/// `assets/app.js`); ticket 05 adds the header/auth section and ticket 13 adds
/// live-region announcements to the same file.
pub const APP_ASSET_PATH: &str = "/assets/app.js";

/// One vendored asset served from the site root and never re-rendered.
struct Asset {
    /// Viewer path, e.g. `/assets/app.js`.
    path: &'static str,
    /// On-disk / S3 object key relative to the site root.
    key: &'static str,
    /// The vendored bytes.
    contents: &'static [u8],
}

/// The pinned HTMX bundle (2.0.4), vendored so the interactivity layer has no
/// network dependency.
const HTMX_ASSET: Asset = Asset {
    path: HTMX_ASSET_PATH,
    key: "assets/htmx.min.js",
    contents: include_bytes!("../assets/htmx.min.js"),
};

/// The read-page behaviour script: vote errors, own-vote painting, and the
/// extension points later tickets append to.
const APP_ASSET: Asset = Asset {
    path: APP_ASSET_PATH,
    key: "assets/app.js",
    contents: include_bytes!("../assets/app.js"),
};

/// The favicon every page links from `<head>`.
const FAVICON_ASSET: Asset = Asset {
    path: crate::shell::FAVICON_PATH,
    key: "favicon.svg",
    contents: include_bytes!("../assets/favicon.svg"),
};

/// The default 1200x630 social card.
const OG_ASSET: Asset = Asset {
    path: crate::shell::DEFAULT_OG_IMAGE,
    key: "assets/og.png",
    contents: include_bytes!("../assets/og.png"),
};

/// Every static asset a render run emits, beside the generated pages.
const ASSETS: &[Asset] = &[HTMX_ASSET, APP_ASSET, FAVICON_ASSET, OG_ASSET];

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

    // CloudFront's custom error responses point at /404.html for both origin
    // 403 and 404 (PRD §4). Emit it with the pages so every deploy includes it.
    let not_found = not_found_html();
    let not_found_file = PathBuf::from(static_file_path(options, "/404"));
    if let Some(parent) = not_found_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&not_found_file, not_found.as_bytes()).await?;
    generated.push(GeneratedPage {
        route_path: "/404".to_owned(),
        file_path: not_found_file,
        bytes: not_found.len(),
    });

    // The interactivity and behaviour scripts are static assets: emit them
    // beside the pages so `aws s3 sync` ships them with the site. They are never
    // re-rendered.
    for asset in ASSETS {
        let file_path = PathBuf::from(&*options.site_root).join(asset.key);
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&file_path, asset.contents).await?;
        generated.push(GeneratedPage {
            route_path: asset.path.to_owned(),
            file_path,
            bytes: asset.contents.len(),
        });
    }

    // Crawler files: one robots.txt and a sitemap of every public route. Both
    // are rebuilt with the site, so they cannot drift from what is published.
    for (path, contents) in crawler_files(site) {
        let file_path = PathBuf::from(&*options.site_root).join(path.trim_start_matches('/'));
        tokio::fs::write(&file_path, contents.as_bytes()).await?;
        generated.push(GeneratedPage {
            route_path: path,
            file_path,
            bytes: contents.len(),
        });
    }

    Ok(generated)
}

/// `robots.txt` and `sitemap.xml` for the current site.
fn crawler_files(site: &Site) -> Vec<(String, String)> {
    let origin = crate::shell::site_origin();
    let mut routes = vec!["/".to_owned(), "/timelines".to_owned()];
    routes.extend(
        site.collection_ids()
            .into_iter()
            .map(|id| format!("/collection/{id}")),
    );
    routes.extend(
        site.timeline_ids()
            .into_iter()
            .map(|id| format!("/timeline/{id}")),
    );

    let urls = routes
        .iter()
        .map(|route| format!("  <url><loc>{}{}</loc></url>\n", origin, route))
        .collect::<String>();
    let sitemap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
         {urls}</urlset>\n"
    );
    let robots = format!(
        "User-agent: *\n\
         Allow: /\n\
         Disallow: /api/\n\
         Sitemap: {origin}/sitemap.xml\n"
    );

    vec![
        ("/robots.txt".to_owned(), robots),
        ("/sitemap.xml".to_owned(), sitemap),
    ]
}

/// One page published to the site bucket by the write path.
#[derive(Clone, Debug)]
pub struct PublishedPage {
    /// Viewer path, e.g. `/timeline/ohio-senate-2026`.
    pub route_path: String,
    /// S3 object key, e.g. `timeline/ohio-senate-2026.html`.
    pub object_key: String,
    /// Size of the uploaded document in bytes.
    pub bytes: usize,
}

/// Viewer path to site-bucket object key. This is the same mapping
/// `static_file_path` uses on disk: `/` becomes `index.html`, and every other
/// route gets a `.html` suffix. The CloudFront viewer-request rewrite adds that
/// same suffix at the edge (PRD §4).
pub fn object_key(route_path: &str) -> String {
    let trimmed = route_path.trim_start_matches('/');
    let path = if trimmed.is_empty() { "index" } else { trimmed };
    format!("{path}.html")
}

/// Render one viewer path to a complete HTML string.
///
/// The write path calls this with the exact page set a mutation dirtied so it
/// does not have to render (or upload) the whole site.
pub async fn render_path(site: &Site, path: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    render_route(site, path).await
}

/// Render every requested viewer path and `PUT` it into the site bucket.
///
/// This is the renderer's S3 write mode: synchronous in the API's write path
/// (PRD §5), with the same no-script document the standalone SSG binary writes
/// to disk. `Cache-Control: max-age=86400` is the missed-invalidation backstop;
/// invalidation is the freshness mechanism.
pub async fn publish_routes(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    site: &Site,
    route_paths: &[String],
) -> Result<Vec<PublishedPage>, Box<dyn Error + Send + Sync>> {
    let mut published = Vec::with_capacity(route_paths.len());
    for route_path in route_paths {
        let html = strip_hydration_bookkeeping(render_route(site, route_path).await?);
        let key = object_key(route_path);
        s3.put_object()
            .bucket(bucket)
            .key(&key)
            .content_type("text/html; charset=utf-8")
            .cache_control("max-age=86400")
            .body(ByteStream::from(html.clone().into_bytes()))
            .send()
            .await?;
        published.push(PublishedPage {
            route_path: route_path.clone(),
            object_key: key,
            bytes: html.len(),
        });
    }
    Ok(published)
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
/// The read page ships no hydration bundle. These two tags only initialize
/// globals a hydration client would consume, so removing them leaves the
/// document with the deferred asset scripts (`/assets/htmx.min.js`,
/// `/assets/app.js`). Any other script tag is preserved.
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

    #[test]
    fn object_keys_match_static_file_path() {
        assert_eq!(object_key("/"), "index.html");
        assert_eq!(
            object_key("/timeline/ohio-senate-2026"),
            "timeline/ohio-senate-2026.html"
        );
        assert_eq!(
            object_key("/collection/elections-2026"),
            "collection/elections-2026.html"
        );
        assert_eq!(object_key("/404"), "404.html");
    }

    #[test]
    fn not_found_page_is_standalone_light_and_links_home() {
        let html = not_found_html();
        assert!(html.contains("Page not found"));
        assert!(html.contains("href=\"/\""));
        assert!(
            html.contains("<header class=\"site-header\">"),
            "the 404 carries the one site header"
        );
        assert!(
            !html.contains("htmx"),
            "the 404 ships no interactivity layer"
        );
        assert!(html.contains("noindex"));
        assert!(
            html.contains("--paper: #faf8f4"),
            "the 404 joins the light theme"
        );
        assert!(!html.contains("color-scheme: dark"), "no dark theme left");
        assert_eq!(
            html.matches("<style").count(),
            1,
            "the 404 uses the one shared stylesheet"
        );
        assert_eq!(html.matches("--paper:").count(), 1);
    }

    #[test]
    fn assets_are_vendored_and_served_from_the_origin_root() {
        assert_eq!(HTMX_ASSET_PATH, "/assets/htmx.min.js");
        assert_eq!(APP_ASSET_PATH, "/assets/app.js");
        assert_eq!(
            ASSETS.len(),
            4,
            "two scripts, the favicon, and the social card ship with the site"
        );
        for asset in ASSETS {
            assert_eq!(
                asset.key,
                asset.path.trim_start_matches('/'),
                "the object key is the served path minus its leading slash"
            );
            assert!(!asset.contents.is_empty(), "{} is vendored", asset.key);
        }
        let htmx = std::str::from_utf8(HTMX_ASSET.contents).expect("htmx is utf-8 text");
        assert!(
            htmx.contains("2.0.4"),
            "the vendored bundle must match the pinned version"
        );
        assert!(
            htmx.contains("htmx"),
            "the vendored bundle must be the htmx runtime"
        );
        assert!(
            std::str::from_utf8(FAVICON_ASSET.contents)
                .expect("svg")
                .contains("<svg"),
            "the favicon is a real SVG"
        );
        assert!(
            OG_ASSET.contents.starts_with(&[0x89, b'P', b'N', b'G']),
            "the social card is a PNG"
        );
    }

    #[test]
    fn app_asset_maps_every_vote_refusal_to_a_visible_message() {
        let script = std::str::from_utf8(APP_ASSET.contents).expect("the app script is utf-8");

        // The status -> message table: one readable sentence per refusal.
        for line in [
            "401: \"Sign in to vote\"",
            "403: \"You cannot vote on your own timeline.\"",
            "409: \"Your vote changed in another tab. Reload the page and try again.\"",
            "429: \"You are voting too quickly.\"",
        ] {
            assert!(
                script.contains(line),
                "the vote error mapping lost {line:?}"
            );
        }
        assert!(
            script.contains("VOTE_MESSAGE_FALLBACK") && script.contains("Your vote was not saved."),
            "an unmapped refusal must still explain itself, never a silent no-op"
        );
        assert!(
            script.contains("Retry-After"),
            "a rate limit honours the Retry-After header"
        );
        assert!(
            script.contains("Try again in {seconds} seconds."),
            "the rate-limit message carries the server's wait"
        );
    }

    #[test]
    fn app_asset_offers_a_sign_in_prompt_that_returns_to_this_timeline() {
        let script = std::str::from_utf8(APP_ASSET.contents).expect("the app script is utf-8");

        assert!(
            script.contains("/api/auth/login?next="),
            "a signed-out vote links to the login route"
        );
        assert!(
            script.contains("window.location.pathname + window.location.hash"),
            "the reader returns to the same path and anchor"
        );
        assert!(
            script.contains("encodeURIComponent"),
            "the return path is encoded"
        );
        assert!(
            script.contains(".vote__status"),
            "the explanation lands in the widget's status element"
        );
    }

    #[test]
    fn app_asset_paints_own_votes_and_never_swaps_a_raw_body() {
        let script = std::str::from_utf8(APP_ASSET.contents).expect("the app script is utf-8");

        assert!(
            script.contains("/api/timeline/") && script.contains("/votes"),
            "one own-vote read per Timeline page"
        );
        assert!(
            script.contains("data-my-vote"),
            "own votes are painted onto the shared attribute"
        );
        assert!(
            script.contains("aria-pressed"),
            "the pressed state reaches assistive tech"
        );
        assert!(
            script.contains("setAttribute(\"data-my-vote\""),
            "the chosen direction is marked, not replaced"
        );
        assert!(
            !script.contains("innerHTML =") && !script.contains(".innerHTML"),
            "a refusal body is never swapped in as HTML"
        );
        assert!(
            script.contains("htmx:responseError") && script.contains("htmx:sendError"),
            "both a refused request and a dead connection are explained"
        );
    }

    #[test]
    fn app_asset_keeps_named_extension_points_for_later_tickets() {
        let script = std::str::from_utf8(APP_ASSET.contents).expect("the app script is utf-8");

        assert!(
            script.contains("Account chrome (ticket 05)"),
            "ticket 05's account chrome lives in its own named section"
        );
        assert!(
            script.contains("Live regions (ticket 13)"),
            "ticket 13 appends announcements here"
        );
    }
}
