//! What a reader gets from one Timeline page: the collection eyebrow, the hero
//! meta (author, updated date, Event count), prose Event dates with the machine
//! value kept, a stable anchor per Event, distinguishable Source chips, and the
//! way out at the end of the page.
//!
//! These tests render through the real router (`render_path`) from an in-memory
//! `Site`, so they pin the delivered HTML rather than an internal helper.

use data::Timestamp;
use data::model::{Collection, Event, Significance, Timeline};
use renderer::{Site, TimelinePage, init_executor, render_path};

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

fn event(event_id: &str, date: &str, title: &str, sources: &[&str]) -> Event {
    let timestamp_value = if date.len() == 10 {
        format!("{date}T00:00:00Z")
    } else {
        date.to_owned()
    };
    Event {
        timeline_id: "ohio-senate-2026".to_owned(),
        event_id: event_id.to_owned(),
        timestamp: timestamp(&timestamp_value),
        date: date.to_owned(),
        title: title.to_owned(),
        description: format!("{title} description."),
        sources: sources.iter().map(|source| (*source).to_owned()).collect(),
        significance: Significance::Regular,
        upvotes: 3,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        created_at: timestamp("2026-01-01T00:00:00Z"),
        created_by: "demo-author".to_owned(),
        last_edited_by: None,
        last_edit_suggestion_id: None,
    }
}

#[allow(clippy::too_many_arguments)] // fixed, readable test fixture builder
fn page(
    id: &str,
    title: &str,
    owner_id: &str,
    collection: Option<&str>,
    collection_title: Option<&str>,
    owner_name: Option<&str>,
    updated_at: &str,
    events: Vec<Event>,
) -> TimelinePage {
    TimelinePage {
        timeline: Timeline {
            id: id.to_owned(),
            title: title.to_owned(),
            description: format!("{title} deck."),
            owner_id: owner_id.to_owned(),
            collection_id: collection.map(str::to_owned),
            event_count: events.len() as i64,
            upvotes: 5,
            downvotes: 1,
            created_at: timestamp("2026-01-01T00:00:00Z"),
            updated_at: timestamp(updated_at),
            report_count: 0,
            flagged: false,
            cover_image: None,
        },
        events,
        owner_name: owner_name.map(str::to_owned),
        collection_title: collection_title.map(str::to_owned),
    }
}

fn collection(id: &str, title: &str) -> Collection {
    Collection {
        id: id.to_owned(),
        title: title.to_owned(),
        description: format!("{title} description"),
        parent_collection_id: None,
        created_at: timestamp("2026-01-01T00:00:00Z"),
        created_by: "demo-author".to_owned(),
    }
}

/// The read page under test: collected, three Events (two of them citing the
/// same host), one sibling Timeline in the same collection, one outside it.
fn read_site() -> Site {
    let covered = page(
        "ohio-senate-2026",
        "Ohio Senate 2026",
        "01H8ZK4Q7X9V3M2N6P5R8T1WYB",
        Some("elections-2026"),
        Some("Elections 2026"),
        Some("Demo Author"),
        "2026-11-08T21:32:00.000Z",
        vec![
            event(
                "filing-deadline",
                "2026-02-04",
                "Candidate filing deadline",
                &["https://www.ohiosos.gov/elections/candidates/"],
            ),
            event(
                "primary-day",
                "2026-05-05",
                "Primary election day",
                &[
                    "https://www.ohiosos.gov/elections/",
                    "https://www.ohiosos.gov/results/primary/",
                    "https://example.org/analysis",
                ],
            ),
            event(
                "general-day",
                "2026-11-03T09:00:00Z",
                "General election day",
                &["https://www.ohiosos.gov/elections/results/"],
            ),
        ],
    );
    let sibling = page(
        "governor-2026",
        "Ohio Governor 2026",
        "demo-author",
        Some("elections-2026"),
        Some("Elections 2026"),
        Some("Demo Author"),
        "2026-10-01T00:00:00.000Z",
        vec![event("only", "2026-10-01", "One thing", &[])],
    );
    let elsewhere = page(
        "city-council-2026",
        "City Council 2026",
        "demo-author",
        Some("local-2026"),
        Some("Local 2026"),
        Some("Demo Author"),
        "2026-09-01T00:00:00.000Z",
        vec![],
    );
    Site {
        pages: vec![covered, sibling, elsewhere],
        collections: vec![
            collection("elections-2026", "Elections 2026"),
            collection("local-2026", "Local 2026"),
        ],
    }
}

/// A Timeline with no collection and no surviving ACCOUNT row.
fn uncollected_site() -> Site {
    Site {
        pages: vec![page(
            "no-cover-timeline",
            "A Timeline Without a Cover",
            "01H8ZK4Q7X9V3M2N6P5R8T1WYB",
            None,
            None,
            None,
            "2026-03-01T00:00:00.000Z",
            vec![event(
                "only-event",
                "2026-03-01",
                "One dated fact",
                &["https://example.com/one"],
            )],
        )],
        collections: Vec::new(),
    }
}

async fn render(site: &Site, timeline_id: &str) -> String {
    init_executor();
    render_path(site, &format!("/timeline/{timeline_id}"))
        .await
        .expect("the timeline page renders")
}

/// The hero header, sliced out the way the API's `extract_hero` finds it.
fn hero(html: &str) -> &str {
    let class_at = html.find("class=\"hero").expect("the document has a hero");
    let start = html[..class_at]
        .rfind("<header")
        .expect("the hero class sits on a header");
    let end = start + html[start..].find("</header>").expect("the hero closes") + "</header>".len();
    &html[start..end]
}

/// The end-of-page block: related Timelines plus the way Home.
fn page_end(html: &str) -> &str {
    let start = html
        .find("<footer class=\"page-end")
        .expect("the page ends with the page-end block");
    let end =
        start + html[start..].find("</footer>").expect("the block closes") + "</footer>".len();
    &html[start..end]
}

/// Headers whose class carries the hero marker — the one `extract_hero` slices
/// out. Counting this way keeps the assertion honest once the shell adds its
/// own `<header class="site-header">`.
fn hero_header_count(html: &str) -> usize {
    html.match_indices("<header")
        .filter(|(start, _)| {
            let end = html[*start..]
                .find('>')
                .map(|offset| start + offset)
                .unwrap_or(html.len());
            let opening_tag = &html[*start..=end];
            opening_tag.contains("class=\"hero ") || opening_tag.contains("class=\"hero\"")
        })
        .count()
}

#[tokio::test]
async fn a_collected_timeline_links_its_collection_eyebrow() {
    let html = render(&read_site(), "ohio-senate-2026").await;
    let hero_html = hero(&html);

    assert!(
        hero_html.contains("class=\"hero__eyebrow\""),
        "the hero keeps its eyebrow: {hero_html}"
    );
    assert!(
        hero_html.contains("href=\"/collection/elections-2026\""),
        "the eyebrow links the collection: {hero_html}"
    );
    assert!(
        hero_html.contains("Elections 2026"),
        "the collection title is the visible label: {hero_html}"
    );
}

#[tokio::test]
async fn an_uncollected_timeline_has_no_eyebrow() {
    let html = render(&uncollected_site(), "no-cover-timeline").await;
    let hero_html = hero(&html);

    assert!(
        !hero_html.contains("hero__eyebrow"),
        "no fake eyebrow without a collection: {hero_html}"
    );
    assert!(
        !hero_html.contains("/collection/"),
        "no collection link without a collection: {hero_html}"
    );
    assert!(
        !hero_html.contains(">Timeline<"),
        "the generic \"Timeline\" label is gone: {hero_html}"
    );
}

#[tokio::test]
async fn the_hero_names_the_author_and_never_the_owner_id() {
    let html = render(&uncollected_site(), "no-cover-timeline").await;
    let hero_html = hero(&html);

    assert!(
        hero_html.contains("the editors"),
        "a missing ACCOUNT row falls back to a human phrase: {hero_html}"
    );
    assert!(
        !html.contains("01H8ZK4Q7X9V3M2N6P5R8T1WYB"),
        "the raw owner id is never printed"
    );

    let named = render(&read_site(), "ohio-senate-2026").await;
    assert!(
        hero(&named).contains("Demo Author"),
        "a known author is named: {}",
        hero(&named)
    );
}

#[tokio::test]
async fn the_hero_shows_the_updated_date_and_a_pluralized_event_count() {
    let html = render(&read_site(), "ohio-senate-2026").await;
    let hero_html = hero(&html);

    assert!(
        hero_html.contains("Updated 8 November 2026"),
        "the hero says when it last changed: {hero_html}"
    );
    assert!(
        hero_html.contains("datetime=\"2026-11-08T21:32:00.000Z\""),
        "the updated date stays machine-readable: {hero_html}"
    );
    assert!(
        hero_html.contains("3 events"),
        "the hero counts the Events: {hero_html}"
    );

    let single = render(&uncollected_site(), "no-cover-timeline").await;
    let single_hero_html = hero(&single);
    assert!(
        single_hero_html.contains("1 event") && !single_hero_html.contains("1 events"),
        "one Event is not \"1 events\": {single_hero_html}"
    );
}

#[tokio::test]
async fn event_dates_read_as_prose_and_keep_the_iso_value() {
    let html = render(&read_site(), "ohio-senate-2026").await;

    assert!(html.contains("4 February 2026"), "the date reads as prose");
    assert!(
        html.contains("datetime=\"2026-02-04\""),
        "the ISO date is still in the datetime attribute"
    );
    assert!(
        html.contains("3 November 2026"),
        "a date-time Event reads as its day"
    );
    assert!(
        html.contains("datetime=\"2026-11-03T09:00:00Z\""),
        "a date-time value stays whole in datetime"
    );
}

#[tokio::test]
async fn every_event_card_carries_a_stable_anchor() {
    let html = render(&read_site(), "ohio-senate-2026").await;

    assert!(
        html.contains("id=\"event-filing-deadline\""),
        "the card is addressable"
    );
    assert!(
        html.contains("href=\"#event-filing-deadline\""),
        "the visible anchor points at its own card"
    );
    assert!(
        html.contains("aria-label=\"Link to this event\""),
        "the anchor says what it does"
    );
    assert!(
        html.contains("id=\"event-general-day\""),
        "every card is addressable, not just the first"
    );
}

#[tokio::test]
async fn sources_on_the_same_host_are_numbered_and_the_new_tab_is_disclosed() {
    let html = render(&read_site(), "ohio-senate-2026").await;

    assert!(
        html.contains("ohiosos.gov (1)") && html.contains("ohiosos.gov (2)"),
        "two Sources on one host are distinguishable"
    );
    assert!(
        html.contains("title=\"https://www.ohiosos.gov/results/primary/\""),
        "each chip carries its full URL as a tooltip"
    );
    assert!(
        html.contains("href=\"https://example.org/analysis\"") && !html.contains("example.org (1)"),
        "a host cited once is not numbered"
    );
    assert!(
        html.contains("opens in a new tab"),
        "opening a new tab is disclosed to screen readers"
    );
    assert!(
        html.contains("target=\"_blank\""),
        "http Sources still open in a new tab"
    );
}

#[tokio::test]
async fn the_end_of_the_page_lists_the_collections_other_timelines() {
    let html = render(&read_site(), "ohio-senate-2026").await;
    let end = page_end(&html);

    assert!(
        end.contains("More in this collection"),
        "the block is named for readers: {end}"
    );
    assert!(
        end.contains("href=\"/timeline/governor-2026\"") && end.contains("Ohio Governor 2026"),
        "a sibling Timeline is listed: {end}"
    );
    assert!(
        !end.contains("href=\"/timeline/ohio-senate-2026\""),
        "the Timeline being read is not its own related link: {end}"
    );
    assert!(
        !end.contains("city-council-2026"),
        "another collection's Timeline is not related: {end}"
    );
    assert!(
        end.contains("href=\"/\""),
        "the dead end is gone: there is a way Home: {end}"
    );
}

#[tokio::test]
async fn an_uncollected_timeline_still_offers_the_way_home() {
    let html = render(&uncollected_site(), "no-cover-timeline").await;
    let end = page_end(&html);

    assert!(
        !end.contains("More in this collection"),
        "no empty related block: {end}"
    );
    assert!(
        end.contains("href=\"/\""),
        "the end of the page still links Home: {end}"
    );
}

#[tokio::test]
async fn the_document_has_exactly_one_hero_header_before_the_reading_column() {
    let html = render(&read_site(), "ohio-senate-2026").await;

    assert_eq!(
        hero_header_count(&html),
        1,
        "the API extracts the hero by finding the one hero header"
    );
    assert!(
        html.find("class=\"hero").expect("hero") < html.find("<main").expect("reading column"),
        "the hero comes before the reading column"
    );
}
