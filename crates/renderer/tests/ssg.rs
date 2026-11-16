//! End-to-end SSG test: build a fixed in-memory site, run the real static-route
//! generation path, and assert the written HTML is the Spine read page.
//!
//! This test deliberately does not touch DynamoDB. It exercises the seam the
//! renderer binary uses after its read: route list -> render -> static file.

use std::path::PathBuf;

use data::Timestamp;
use data::model::{Collection, CoverImage, Event, Significance, Timeline};
use leptos::config::LeptosOptions;
use renderer::{Site, TimelinePage, generate_site, gradient_for, init_executor};

/// The owner ACCOUNT id behind every fixture. No generated page may contain it:
/// a reader sees the owner's display name, or nothing at all.
const OWNER_ACCOUNT_ID: &str = "demo-author";

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

#[allow(clippy::too_many_arguments)] // fixed, readable test fixture builder
fn event(
    event_id: &str,
    timestamp_value: &str,
    date: &str,
    title: &str,
    description: &str,
    sources: &[&str],
    significance: Significance,
    upvotes: i64,
    downvotes: i64,
) -> Event {
    Event {
        timeline_id: "ohio-senate-2026".to_owned(),
        event_id: event_id.to_owned(),
        timestamp: timestamp(timestamp_value),
        date: date.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        sources: sources.iter().map(|s| (*s).to_owned()).collect(),
        significance,
        upvotes,
        downvotes,
        report_count: 0,
        flagged: false,
        created_at: timestamp("2026-01-01T00:00:00Z"),
        created_by: "demo-author".to_owned(),
        last_edited_by: None,
        last_edit_suggestion_id: None,
    }
}

/// A fixed Timeline with a cover, three chronological Events, sources, and
/// baked vote counts. Same shape the seeder writes to floci.
fn covered_page() -> TimelinePage {
    TimelinePage {
        timeline: Timeline {
            id: "ohio-senate-2026".to_owned(),
            title: "Ohio Senate 2026".to_owned(),
            description: "Key dates in the 2026 Ohio Senate election cycle.".to_owned(),
            owner_id: "demo-author".to_owned(),
            collection_id: Some("elections-2026".to_owned()),
            event_count: 3,
            upvotes: 58,
            downvotes: 10,
            created_at: timestamp("2026-01-01T00:00:00Z"),
            updated_at: timestamp("2026-01-02T00:00:00Z"),
            report_count: 0,
            flagged: false,
            published: true,
            cover_image: Some(CoverImage {
                key: "cover/ohio-senate-2026/hero.jpg".to_owned(),
                width: 1600,
                height: 900,
                credit: Some("Photo: Demo Author".to_owned()),
            }),
        },
        events: vec![
            event(
                "filing-deadline",
                "2026-02-04T00:00:00Z",
                "2026-02-04",
                "Candidate filing deadline",
                "Petitions for the 2026 primary had to be filed by 4 p.m.",
                &["https://www.ohiosos.gov/elections/candidates/"],
                Significance::Regular,
                12,
                1,
            ),
            event(
                "primary-day",
                "2026-05-05T00:00:00Z",
                "2026-05-05",
                "Primary election day",
                "Party nominees for Ohio's 33 State Senate districts were decided.",
                &[
                    "https://www.ohiosos.gov/elections/",
                    "https://www.example.org/primary-results",
                ],
                Significance::Crucial,
                34,
                6,
            ),
            event(
                "general-day",
                "2026-11-03T00:00:00Z",
                "2026-11-03",
                "General election day",
                "All 33 State Senate seats were on the ballot.",
                &["https://www.ohiosos.gov/elections/results/"],
                Significance::Regular,
                41,
                3,
            ),
        ],
        owner_name: Some("Demo Author".to_owned()),
        collection_title: Some("Elections 2026".to_owned()),
    }
}

/// A no-cover Timeline: the renderer must use the deterministic gradient.
fn gradient_page() -> TimelinePage {
    let mut page = covered_page();
    page.timeline.id = "no-cover-timeline".to_owned();
    page.timeline.title = "A Timeline Without a Cover".to_owned();
    page.timeline.description = "The hero falls back to a deterministic gradient.".to_owned();
    page.timeline.cover_image = None;
    page.timeline.collection_id = None;
    page.owner_name = None;
    page.collection_title = None;
    page.events = vec![event(
        "only-event",
        "2026-03-01T00:00:00Z",
        "2026-03-01",
        "One dated fact",
        "Rendered on the plain reading column.",
        &["https://example.com/one"],
        Significance::Regular,
        3,
        0,
    )];
    page
}

fn collection(id: &str, title: &str, parent: Option<&str>) -> Collection {
    Collection {
        id: id.to_owned(),
        title: title.to_owned(),
        description: format!("{title} description"),
        parent_collection_id: parent.map(str::to_owned),
        created_at: timestamp("2026-01-01T00:00:00Z"),
        created_by: "demo-author".to_owned(),
    }
}

fn unique_site_root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "timeline-renderer-{name}-{}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn covered_href(page: &TimelinePage) -> String {
    format!(
        "/media/{}",
        page.timeline
            .cover_image
            .as_ref()
            .expect("covered page has a cover")
            .key
    )
}

/// Every generated document comes from the one shell: one `<style>` element
/// (tokens + base + this page's blocks) and one token block.
fn assert_single_stylesheet(name: &str, html: &str) {
    assert!(
        html.contains("<html lang=\"en\">"),
        "{name} is a complete document"
    );
    assert_eq!(
        html.matches("<style").count(),
        1,
        "{name} has exactly one stylesheet"
    );
    assert_eq!(
        html.matches("--paper:").count(),
        1,
        "{name} has exactly one token block"
    );
}

/// The one site header: the site name links Home, the account area's first
/// paint is the anonymous "Sign in" link, the signed-in pieces are present but
/// hidden and empty, and the site script is loaded. No page bakes an identity
/// and no page leaks the owner's account id.
fn assert_site_header(name: &str, html: &str) {
    assert!(
        html.contains("<header class=\"site-header\">"),
        "{name} carries the site header"
    );
    assert!(
        html.contains("<a class=\"site-header__brand\" href=\"/\">Timelines</a>"),
        "{name} links Home from the site name"
    );
    assert!(
        html.contains("<a class=\"site-header__signin\" href=\"/api/auth/login?next=/\" data-sign-in>Sign in</a>"),
        "{name} offers the anonymous sign-in link"
    );
    assert!(
        html.contains("<span class=\"site-header__name\" data-account-name hidden></span>"),
        "{name} shows no identity before the account check"
    );
    assert!(
        html.contains("action=\"/api/auth/logout\""),
        "{name} carries the POST sign-out form"
    );
    assert!(
        html.contains("<script src=\"/assets/app.js\" defer></script>"),
        "{name} loads the one site script"
    );
    assert!(
        !html.contains(OWNER_ACCOUNT_ID),
        "{name} must not leak the owner account id"
    );
}

/// The opening tag of the one add-event action on a read page.
fn add_event_tag(html: &str) -> &str {
    let text = html.find(">Add an event</a>").expect("an add action");
    let start = html[..text].rfind('<').expect("tag start");
    &html[start..text]
}

#[tokio::test]
async fn ssg_writes_the_spine_read_page_without_scripts() {
    init_executor();

    let root = unique_site_root("spine");
    let options = LeptosOptions::builder()
        .output_name("renderer-test")
        .site_root(root.to_string_lossy().to_string())
        .site_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 3000)))
        .build();

    let covered = covered_page();
    let covered_href = covered_href(&covered);
    let site = Site {
        pages: vec![covered, gradient_page()],
        collections: vec![
            collection("elections-2026", "Elections 2026", None),
            collection("senate-races", "Senate Races", Some("elections-2026")),
        ],
    };

    let outputs = generate_site(&site, &options)
        .await
        .expect("static generation succeeds");
    assert_eq!(
        outputs.len(),
        9,
        "two timelines, home, the full list, two collection pages, the 404 page, and the two assets"
    );

    let home = outputs
        .iter()
        .find(|o| o.route_path == "/")
        .expect("home was generated");
    assert!(
        home.file_path.ends_with("index.html"),
        "home lands at index.html, got {}",
        home.file_path.display()
    );
    let home_html = std::fs::read_to_string(&home.file_path).expect("home is readable");
    assert!(
        home_html.contains("Ohio Senate 2026"),
        "home lists timelines"
    );
    assert!(home_html.contains("A Timeline Without a Cover"));
    assert!(home_html.contains("href=\"/timeline/ohio-senate-2026\""));
    assert!(
        home_html.contains("Collections"),
        "home has collection entry points"
    );
    assert!(
        home_html.contains("href=\"/collection/elections-2026\""),
        "home links the top-level collection"
    );
    assert!(
        home_html.contains("Elections 2026"),
        "home shows the collection title"
    );
    assert!(
        home_html.contains("Recently updated"),
        "home groups timelines by recency"
    );
    // The header is the whole document's chrome: site name + anonymous account
    // area, first paint only, and it precedes the page body.
    assert_site_header("home", &home_html);
    assert!(
        home_html.find("<header class=\"site-header\">")
            < home_html.find("<header class=\"index__hero\">"),
        "the site header comes before the page body"
    );
    assert!(
        home_html.contains("href=\"/timelines\"") && home_html.contains("See all timelines"),
        "home never hides the rest of the Timelines without a way to see them"
    );
    assert_single_stylesheet("home", &home_html);

    let all = outputs
        .iter()
        .find(|o| o.route_path == "/timelines")
        .expect("the full list was generated");
    assert!(
        all.file_path.ends_with("timelines.html"),
        "the full list lands at timelines.html, got {}",
        all.file_path.display()
    );
    let all_html = std::fs::read_to_string(&all.file_path).expect("full list is readable");
    assert!(all_html.contains("Ohio Senate 2026"));
    assert!(all_html.contains("A Timeline Without a Cover"));
    assert!(
        all_html.contains("Feb - Nov 2026"),
        "an entry carries the story's date span"
    );
    assert!(
        all_html.contains("Updated 2 January 2026"),
        "an entry carries when it last changed"
    );
    assert!(
        all_html.contains(&format!("src=\"{covered_href}\"")),
        "an entry reuses the hero's /media/<key> cover"
    );
    assert!(
        all_html.contains("index__thumb--gradient"),
        "a cover-less Timeline keeps its deterministic gradient thumbnail"
    );
    assert!(
        all_html.contains("href=\"/\"") && all_html.contains("Back to Home"),
        "the full list is not a dead end"
    );
    assert!(
        !all_html.contains("__RESOLVED_RESOURCES") && all_html.contains("/assets/app.js"),
        "the full list ships the site script and no hydration bundle"
    );
    assert_single_stylesheet("full list", &all_html);

    let collection = outputs
        .iter()
        .find(|o| o.route_path == "/collection/elections-2026")
        .expect("collection page was generated");
    let collection_html =
        std::fs::read_to_string(&collection.file_path).expect("collection is readable");
    assert!(
        collection_html.contains("Elections 2026"),
        "collection title is the eyebrow/heading"
    );
    assert!(collection_html.contains("Ohio Senate 2026"));
    assert!(
        collection_html.contains("href=\"/collection/senate-races\""),
        "collection lists its child collections"
    );
    assert!(
        collection_html.contains("Senate Races"),
        "child collection title is shown"
    );
    assert!(
        collection_html.contains("index__group-title"),
        "timelines are rendered in groups"
    );
    assert!(
        collection_html.contains(">2026</h3>"),
        "the group heading is the update year"
    );
    // The owner panel below the hero may list every Timeline as a candidate
    // for membership; the public list still only carries this Collection's own.
    let public = &collection_html[..collection_html
        .find("class=\"manage\"")
        .unwrap_or(collection_html.len())];
    assert!(
        !public.contains("A Timeline Without a Cover"),
        "the uncollected timeline is not in this collection"
    );
    assert_site_header("collection", &collection_html);
    assert_single_stylesheet("collection", &collection_html);

    let child = outputs
        .iter()
        .find(|o| o.route_path == "/collection/senate-races")
        .expect("nested collection page was generated");
    let child_html = std::fs::read_to_string(&child.file_path).expect("child is readable");
    assert!(child_html.contains("Senate Races"));
    assert!(
        child_html.contains("No Timelines in Senate Races yet."),
        "an empty nested collection names itself"
    );
    assert!(
        child_html.contains("href=\"/api/timeline\""),
        "an empty collection offers the next action"
    );
    assert_site_header("child collection", &child_html);
    assert_single_stylesheet("child collection", &child_html);

    let not_found = outputs
        .iter()
        .find(|o| o.route_path == "/404")
        .expect("custom error page was generated");
    assert!(
        not_found.file_path.ends_with("404.html"),
        "error page lands at 404.html, got {}",
        not_found.file_path.display()
    );
    let not_found_html =
        std::fs::read_to_string(&not_found.file_path).expect("error page is readable");
    assert!(
        not_found_html.contains("Page not found"),
        "error page is the custom document"
    );
    assert!(
        not_found_html.contains("href=\"/\""),
        "the 404 offers a way Home"
    );
    assert!(
        not_found_html.contains("--paper: #faf8f4")
            && !not_found_html.contains("color-scheme: dark"),
        "the 404 joins a light theme"
    );
    assert!(not_found_html.contains("noindex"), "the 404 stays noindex");
    assert!(
        !not_found_html.contains("htmx"),
        "the 404 ships no interactivity layer"
    );
    assert_site_header("404", &not_found_html);
    assert_single_stylesheet("404", &not_found_html);

    let htmx_asset = outputs
        .iter()
        .find(|o| o.route_path == "/assets/htmx.min.js")
        .expect("the HTMX asset was generated");
    assert!(
        htmx_asset
            .file_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("assets/htmx.min.js"),
        "the HTMX asset lands at assets/htmx.min.js, got {}",
        htmx_asset.file_path.display()
    );
    assert!(
        std::fs::read_to_string(&htmx_asset.file_path)
            .expect("htmx asset is readable")
            .contains("htmx"),
        "the HTMX asset is the vendored runtime"
    );

    let app_asset = outputs
        .iter()
        .find(|o| o.route_path == "/assets/app.js")
        .expect("the behaviour asset was generated");
    assert!(
        app_asset
            .file_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("assets/app.js"),
        "the behaviour script lands beside the HTMX bundle, got {}",
        app_asset.file_path.display()
    );
    let script_source =
        std::fs::read_to_string(&app_asset.file_path).expect("app asset is readable");
    assert!(
        script_source.contains("Sign in to vote"),
        "the behaviour script carries the vote refusal messages"
    );
    assert!(
        script_source.contains("/api/auth/me"),
        "the site script paints the account area from /api/auth/me"
    );
    assert!(
        script_source.contains("/access"),
        "the site script asks the access probe before revealing owner controls"
    );

    let covered_output = outputs
        .iter()
        .find(|o| o.route_path == "/timeline/ohio-senate-2026")
        .expect("covered timeline was generated");
    assert!(
        covered_output
            .file_path
            .ends_with("timeline/ohio-senate-2026.html")
            || covered_output
                .file_path
                .ends_with("timeline\\ohio-senate-2026.html"),
        "static_file_path puts the page at timeline/<id>.html, got {}",
        covered_output.file_path.display()
    );

    let html =
        std::fs::read_to_string(&covered_output.file_path).expect("generated page is readable");
    println!(
        "generated {} -> {} bytes",
        covered_output.file_path.display(),
        html.len()
    );

    assert!(
        html.starts_with("<!DOCTYPE html>"),
        "page is a complete document"
    );

    // Hero: cover image, dimensions, credit, collection eyebrow, title, deck,
    // byline, and Timeline-level baked vote count.
    assert!(html.contains("class=\"hero hero--cover\""), "cover hero");
    assert!(
        html.contains(&format!("src=\"{covered_href}\"")),
        "cover uses /media/<key>"
    );
    assert!(html.contains("width=\"1600\""), "cover width attribute");
    assert!(html.contains("height=\"900\""), "cover height attribute");
    assert!(html.contains("Photo: Demo Author"), "cover credit");
    assert!(html.contains("Elections 2026"), "collection eyebrow");
    assert!(html.contains("Ohio Senate 2026"), "hero title");
    assert!(
        html.contains("Key dates in the 2026 Ohio Senate election cycle."),
        "serif deck"
    );
    assert!(html.contains("Demo Author"), "byline");
    assert!(html.contains("data-up=\"58\""), "baked timeline upvotes");
    assert!(
        html.contains("data-down=\"10\""),
        "baked timeline downvotes"
    );

    // Events appear in chronological order, with date kickers, titles,
    // descriptions, source chips, Event-level votes, and edit affordances.
    let filing = html.find("Candidate filing deadline").expect("event 1");
    let primary = html.find("Primary election day").expect("event 2");
    let general = html.find("General election day").expect("event 3");
    assert!(
        filing < primary && primary < general,
        "events render in chronological order"
    );

    assert!(html.contains("datetime=\"2026-02-04\""), "date kicker");
    assert!(
        html.contains("https://www.ohiosos.gov/elections/candidates/"),
        "source chip href"
    );
    assert!(html.contains("class=\"source-chip\""), "source chip class");
    assert!(html.contains("ohiosos.gov"), "source chip label");
    assert!(html.contains("data-up=\"12\""), "baked event upvotes");
    assert!(html.contains("data-down=\"1\""), "baked event downvotes");
    assert!(
        html.contains("Suggest an edit"),
        "per-event edit affordance"
    );
    assert!(
        html.contains("spine__card--crucial"),
        "crucial event keeps its accent ring"
    );

    // Exactly one add action, at the end of the Timeline, and owner-only: it
    // ships hidden and the site script reveals it after the access check.
    assert_eq!(
        html.matches("Add an event").count(),
        1,
        "one add action on the whole page"
    );
    assert!(
        !html.contains("Add an event here"),
        "no per-position pills: the model cannot insert between Events"
    );
    let add = add_event_tag(&html);
    assert!(
        add.contains("href=\"/api/timeline/ohio-senate-2026/events/new\""),
        "the action opens the append form: {add}"
    );
    assert!(add.contains("hidden"), "it ships hidden: {add}");
    assert!(add.contains("data-owner-only"), "it is owner-gated: {add}");
    assert!(
        add.contains("data-timeline-id=\"ohio-senate-2026\""),
        "it names the Timeline it owns: {add}"
    );
    assert_site_header("covered timeline", &html);

    // Read pages ship HTMX as the interactivity layer (PRD §4) but no Leptos
    // hydration bundle: no wasm and no resource bookkeeping scripts.
    assert!(
        html.contains("/assets/htmx.min.js"),
        "read page wires HTMX from the local asset"
    );
    assert!(
        html.contains("/assets/app.js"),
        "read page wires the vote behaviour script from the local asset"
    );
    assert!(
        html.contains("hx-post=\"/api/timeline/ohio-senate-2026/vote\""),
        "timeline vote is HTMX-wired"
    );
    assert!(
        html.contains("hx-post=\"/api/timeline/ohio-senate-2026/event/filing-deadline/vote\""),
        "event vote is HTMX-wired"
    );
    assert!(
        html.contains("hx-vals"),
        "vote buttons carry their direction"
    );
    assert!(
        html.contains("data-my-vote=\"none\""),
        "a static page paints the neutral own-vote state"
    );
    assert!(
        html.contains("aria-pressed=\"false\""),
        "the direction buttons expose an unpressed state"
    );
    assert!(
        html.contains("role=\"group\"")
            && html.contains("hx-target=\"this\" hx-swap=\"outerHTML\""),
        "each widget is a labelled group that HTMX can swap in place"
    );
    assert!(
        html.contains("hidden>×</button>"),
        "the clear action is hidden until the reader has voted"
    );
    assert!(
        html.contains("Is this timeline accurate?") && html.contains("Is this event accurate?"),
        "the prompts are neutral, not ratings"
    );
    assert!(!html.contains("wasm"), "no wasm hydration bundle");
    assert!(
        !html.contains("__RESOLVED_RESOURCES"),
        "no Leptos hydration bookkeeping"
    );
    assert_single_stylesheet("covered timeline", &html);

    // The no-cover page falls back to the deterministic gradient.
    let gradient_output = outputs
        .iter()
        .find(|o| o.route_path == "/timeline/no-cover-timeline")
        .expect("gradient timeline was generated");
    let gradient_html =
        std::fs::read_to_string(&gradient_output.file_path).expect("gradient page is readable");
    assert!(
        gradient_html.contains("hero hero--gradient"),
        "gradient hero class"
    );
    assert!(
        gradient_html.contains("--gradient-from:"),
        "inline gradient CSS"
    );
    assert!(
        gradient_html.contains(&format!(
            "data-gradient=\"{}\"",
            gradient_for("no-cover-timeline").hue
        )),
        "gradient is keyed on the Timeline id"
    );
    assert!(
        !gradient_html.contains("<img class=\"hero__cover\""),
        "no cover image on the gradient hero"
    );
    assert!(
        gradient_html.contains("/assets/htmx.min.js"),
        "gradient timeline page wires HTMX too"
    );
    assert!(
        gradient_html.contains("the editors"),
        "a missing owner row falls back to a human byline, never the account id"
    );
    assert_site_header("gradient timeline", &gradient_html);
    assert_single_stylesheet("gradient timeline", &gradient_html);

    if std::env::var_os("KEEP_RENDERED_SITE").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[tokio::test]
async fn an_empty_site_renders_home_with_an_empty_state_and_a_publish_call_to_action() {
    init_executor();

    let root = unique_site_root("empty");
    let options = LeptosOptions::builder()
        .output_name("renderer-test")
        .site_root(root.to_string_lossy().to_string())
        .site_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 3000)))
        .build();

    // A fresh environment: no Timelines at all, one Collection row.
    let site = Site {
        pages: Vec::new(),
        collections: vec![collection("elections-2026", "Elections 2026", None)],
    };

    let outputs = generate_site(&site, &options)
        .await
        .expect("an empty site is still a site: the render must not fail");
    assert_eq!(
        outputs.len(),
        6,
        "home, the full list, one collection page, the 404 page, HTMX, and the site script"
    );

    let home = outputs
        .iter()
        .find(|o| o.route_path == "/")
        .expect("home was generated for an empty site");
    let home_html = std::fs::read_to_string(&home.file_path).expect("home is readable");
    assert!(
        home_html.contains("No Timelines yet"),
        "home explains that nothing is published yet: {home_html}"
    );
    assert!(
        home_html.contains("href=\"/api/timeline\""),
        "home offers the publish entry point"
    );
    assert!(
        home_html.contains("Publish the first Timeline"),
        "the call to action says what it does"
    );
    assert!(
        !home_html.contains("Recently updated"),
        "an empty site does not fake a recency list"
    );
    assert_site_header("empty home", &home_html);
    assert_single_stylesheet("empty home", &home_html);

    let collection_page = outputs
        .iter()
        .find(|o| o.route_path == "/collection/elections-2026")
        .expect("an empty collection still gets its page");
    let collection_html =
        std::fs::read_to_string(&collection_page.file_path).expect("collection is readable");
    assert!(
        collection_html.contains("No Timelines in Elections 2026 yet."),
        "the collection empty state names the collection"
    );
    assert!(
        collection_html.contains("href=\"/api/timeline\""),
        "the empty collection points at publishing too"
    );
    assert_site_header("empty collection", &collection_html);
    assert_single_stylesheet("empty collection", &collection_html);

    if std::env::var_os("KEEP_RENDERED_SITE").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn gradient_is_deterministic_and_keyed_on_the_timeline_id() {
    let first = gradient_for("ohio-senate-2026");
    let again = gradient_for("ohio-senate-2026");
    let other = gradient_for("no-cover-timeline");

    assert_eq!(first, again, "same id gives the same gradient");
    assert_ne!(first.hue, other.hue, "different ids give different hues");
    assert!((0..360).contains(&first.hue), "hue stays in range");
    assert!((0..360).contains(&first.accent), "accent stays in range");
    assert_ne!(first.hue, first.accent, "the gradient has two tones");
}
