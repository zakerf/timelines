//! End-to-end SSG test: build a fixed in-memory site, run the real static-route
//! generation path, and assert the written HTML is the Spine read page.
//!
//! This test deliberately does not touch DynamoDB. It exercises the seam the
//! renderer binary uses after its read: route list -> render -> static file.

use std::path::PathBuf;

use data::Timestamp;
use data::model::{CoverImage, Emphasis, Event, Timeline};
use leptos::config::LeptosOptions;
use renderer::{Site, TimelinePage, generate_site, gradient_for, init_executor};

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
    emphasis: Emphasis,
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
        emphasis,
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
                Emphasis::Standard,
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
                Emphasis::Featured,
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
                Emphasis::Standard,
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
    page.owner_name = None;
    page.collection_title = None;
    page.events = vec![event(
        "only-event",
        "2026-03-01T00:00:00Z",
        "2026-03-01",
        "One dated fact",
        "Rendered on the plain reading column.",
        &["https://example.com/one"],
        Emphasis::Standard,
        3,
        0,
    )];
    page
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
    };

    let outputs = generate_site(&site, &options)
        .await
        .expect("static generation succeeds");
    assert_eq!(
        outputs.len(),
        3,
        "one static page per timeline plus the 404 page"
    );

    let not_found = outputs
        .iter()
        .find(|o| o.route_path == "/404")
        .expect("custom error page was generated");
    assert!(
        not_found.file_path.ends_with("404.html"),
        "error page lands at 404.html, got {}",
        not_found.file_path.display()
    );
    assert!(
        std::fs::read_to_string(&not_found.file_path)
            .expect("error page is readable")
            .contains("Page not found"),
        "error page is the custom document"
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
        html.contains("spine__card--featured"),
        "featured event keeps its accent ring"
    );

    // Add-event pills between cards and at the end.
    assert_eq!(
        html.matches("+ Add an event here").count(),
        2,
        "a pill between each pair of cards"
    );
    assert_eq!(
        html.matches("+ Add an event at the end").count(),
        1,
        "one closing pill"
    );

    // Static read pages ship no hydration bundle and no scripts at all.
    assert!(!html.contains("<script"), "no script tags in static output");
    assert!(!html.contains("wasm"), "no wasm hydration bundle");

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
        !gradient_html.contains("<script"),
        "gradient page has no scripts"
    );

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
