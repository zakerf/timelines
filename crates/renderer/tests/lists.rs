//! Browsing surfaces: what a reader gets from Home, a Collection page, and the
//! full `/timelines` list.
//!
//! Every assertion renders through the real router (`render_path`) from an
//! in-memory `Site`, so the tests pin delivered HTML rather than an internal
//! helper.

use data::Timestamp;
use data::model::{Collection, CoverImage, Event, Significance, Timeline};
use renderer::{Site, TimelinePage, init_executor, render_path};

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

fn event(timeline_id: &str, date: &str, title: &str) -> Event {
    let stamp = if date.len() == 10 {
        format!("{date}T00:00:00Z")
    } else {
        date.to_owned()
    };
    Event {
        timeline_id: timeline_id.to_owned(),
        event_id: format!("{timeline_id}-{}", date.replace(['-', ':', 'T', 'Z'], "")),
        timestamp: timestamp(&stamp),
        date: date.to_owned(),
        title: title.to_owned(),
        description: format!("{title} description."),
        sources: vec!["https://example.org/source".to_owned()],
        significance: Significance::Regular,
        upvotes: 1,
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
    collection: Option<&str>,
    owner_name: Option<&str>,
    updated_at: &str,
    cover: Option<CoverImage>,
    events: Vec<Event>,
) -> TimelinePage {
    TimelinePage {
        timeline: Timeline {
            id: id.to_owned(),
            title: title.to_owned(),
            description: format!("{title} deck."),
            owner_id: "01H8ZK4Q7X9V3M2N6P5R8T1WYB".to_owned(),
            collection_id: collection.map(str::to_owned),
            event_count: events.len() as i64,
            upvotes: 5,
            downvotes: 1,
            created_at: timestamp("2026-01-01T00:00:00Z"),
            updated_at: timestamp(updated_at),
            report_count: 0,
            flagged: false,
            cover_image: cover,
        },
        events,
        owner_name: owner_name.map(str::to_owned),
        collection_title: collection.map(|id| match id {
            "elections-2026" => "Elections 2026".to_owned(),
            "local-2026" => "Local 2026".to_owned(),
            other => other.to_owned(),
        }),
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

fn cover(key: &str) -> CoverImage {
    CoverImage {
        key: key.to_owned(),
        width: 1600,
        height: 900,
        credit: Some("Photo: Demo Author".to_owned()),
    }
}

/// Two collected Timelines: one covered with a real story span, one on its
/// gradient with a single Event and no surviving ACCOUNT row.
fn list_site() -> Site {
    let senate = page(
        "ohio-senate-2026",
        "Ohio Senate 2026",
        Some("elections-2026"),
        Some("Demo Author"),
        "2026-11-08T21:32:00.000Z",
        Some(cover("cover/ohio-senate-2026/hero.jpg")),
        vec![
            event(
                "ohio-senate-2026",
                "2026-02-04",
                "Candidate filing deadline",
            ),
            event("ohio-senate-2026", "2026-11-03", "General election day"),
        ],
    );
    let governor = page(
        "governor-2026",
        "Ohio Governor 2026",
        Some("elections-2026"),
        None,
        "2026-10-01T00:00:00.000Z",
        None,
        vec![event("governor-2026", "2026-10-01", "One dated fact")],
    );
    Site {
        pages: vec![senate, governor],
        collections: vec![collection("elections-2026", "Elections 2026")],
    }
}

/// A second fixture collection whose story years deliberately disagree with
/// the edit years, plus one Timeline that has no Events at all.
fn story_site() -> Site {
    let senate = page(
        "ohio-senate-2026",
        "Ohio Senate 2026",
        Some("elections-2026"),
        Some("Demo Author"),
        "2024-05-01T00:00:00Z",
        None,
        vec![
            event(
                "ohio-senate-2026",
                "2026-02-04",
                "Candidate filing deadline",
            ),
            event("ohio-senate-2026", "2026-11-03", "General election day"),
        ],
    );
    let primary = page(
        "ohio-primary-2026",
        "Ohio Primary 2026",
        Some("elections-2026"),
        Some("Demo Author"),
        "2020-01-01T00:00:00Z",
        None,
        vec![event("ohio-primary-2026", "2026-02-04", "Filing opens")],
    );
    let budget = page(
        "city-budget-2025",
        "City Budget 2025",
        Some("elections-2026"),
        Some("Demo Author"),
        "2026-06-01T00:00:00Z",
        None,
        vec![event("city-budget-2025", "2025-03-10", "Budget passed")],
    );
    let undated = page(
        "no-events-yet",
        "A Timeline With No Events",
        Some("elections-2026"),
        Some("Demo Author"),
        "2026-01-01T00:00:00Z",
        None,
        Vec::new(),
    );
    Site {
        pages: vec![senate, primary, budget, undated],
        collections: vec![collection("elections-2026", "Elections 2026")],
    }
}

async fn render(site: &Site, path: &str) -> String {
    init_executor();
    render_path(site, path)
        .await
        .unwrap_or_else(|error| panic!("{path} renders: {error}"))
}

/// The `<li class="index__item">` that holds the Timeline's title link.
fn entry<'a>(html: &'a str, timeline_id: &str) -> &'a str {
    let link = format!("href=\"/timeline/{timeline_id}\"");
    let at = html
        .find(&link)
        .unwrap_or_else(|| panic!("{timeline_id} is listed"));
    let start = html[..at]
        .rfind("<li")
        .expect("the link sits in a list item");
    let end = start + html[start..].find("</li>").expect("the item closes") + "</li>".len();
    &html[start..end]
}

/// One Collection group, from its heading to the end of its section.
fn group<'a>(html: &'a str, label: &str) -> &'a str {
    let heading = format!("<h3 class=\"index__group-title\">{label}</h3>");
    let start = html
        .find(&heading)
        .unwrap_or_else(|| panic!("the {label:?} group exists"));
    let end = start
        + html[start..]
            .find("</section>")
            .expect("the group's section closes")
        + "</section>".len();
    &html[start..end]
}

#[tokio::test]
async fn an_entry_shows_cover_or_gradient_and_the_facts_a_reader_chooses_with() {
    let html = render(&list_site(), "/collection/elections-2026").await;

    let covered = entry(&html, "ohio-senate-2026");
    assert!(
        covered.contains("src=\"/media/cover/ohio-senate-2026/hero.jpg\""),
        "the cover is the thumbnail, from /media/<key>: {covered}"
    );
    assert!(
        covered.contains("width=\"1600\"") && covered.contains("height=\"900\""),
        "the thumbnail keeps its intrinsic size: {covered}"
    );
    assert!(
        covered.contains("loading=\"lazy\""),
        "a list thumbnail is not eager: {covered}"
    );
    assert!(
        covered.contains("alt=\"\""),
        "the thumbnail is decorative; the title is adjacent text: {covered}"
    );
    assert!(
        !covered.contains("index__thumb--gradient"),
        "a covered Timeline is not also drawn as a gradient: {covered}"
    );
    assert!(
        covered.contains("Feb - Nov 2026"),
        "the entry shows the story's date span: {covered}"
    );
    assert!(
        covered.contains("Updated 8 November 2026"),
        "the entry shows when it changed: {covered}"
    );
    assert!(
        covered.contains("By Demo Author"),
        "the entry names the author: {covered}"
    );
    assert!(
        covered.contains("2 events") && !covered.contains("2 eventss"),
        "the entry pluralizes its Event count: {covered}"
    );
    assert!(
        !covered.contains("01H8ZK4Q7X9V3M2N6P5R8T1WYB"),
        "the raw owner id is never printed"
    );

    let gradient = entry(&html, "governor-2026");
    assert!(
        gradient.contains("index__thumb--gradient"),
        "a Timeline with no cover keeps its deterministic gradient: {gradient}"
    );
    assert!(
        gradient.contains("--gradient-from:") && gradient.contains("--gradient-to:"),
        "the gradient reuses the hero's inline custom properties: {gradient}"
    );
    assert!(
        gradient.contains("aria-hidden=\"true\""),
        "the gradient is decorative: {gradient}"
    );
    assert!(
        !gradient.contains("<img"),
        "a gradient thumbnail is not an image: {gradient}"
    );
    assert!(
        gradient.contains("1 October 2026"),
        "a single-event story reads as its date: {gradient}"
    );
    assert!(
        gradient.contains("By the editors"),
        "a missing ACCOUNT row falls back to a human phrase: {gradient}"
    );
    assert!(
        gradient.contains("1 event") && !gradient.contains("1 events"),
        "one Event is not \"1 events\": {gradient}"
    );
}

#[tokio::test]
async fn a_collection_groups_by_story_year_not_edit_year() {
    let html = render(&story_site(), "/collection/elections-2026").await;

    let headings: Vec<&str> = html
        .match_indices("<h3 class=\"index__group-title\">")
        .map(|(start, tag)| {
            let text_start = start + tag.len();
            let text_end = text_start
                + html[text_start..]
                    .find("</h3>")
                    .expect("the heading closes");
            &html[text_start..text_end]
        })
        .collect();
    assert_eq!(
        headings,
        ["2026", "2025", "No dates yet"],
        "the headings are the story's years, newest first, and undated work is not hidden"
    );

    let story_2026 = group(&html, "2026");
    assert!(
        story_2026.contains("href=\"/timeline/ohio-senate-2026\""),
        "a 2026 story sits under 2026 even though it was edited in 2024: {story_2026}"
    );
    assert!(
        !story_2026.contains("href=\"/timeline/city-budget-2025\""),
        "the 2025 story is not in the 2026 group: {story_2026}"
    );
    let senate = story_2026
        .find("href=\"/timeline/ohio-senate-2026\"")
        .expect("senate");
    let primary = story_2026
        .find("href=\"/timeline/ohio-primary-2026\"")
        .expect("primary");
    assert!(
        senate < primary,
        "inside a year the latest story date leads: {story_2026}"
    );

    let story_2025 = group(&html, "2025");
    assert!(
        story_2025.contains("href=\"/timeline/city-budget-2025\""),
        "a 2025 story sits under 2025 even though it was edited in 2026: {story_2025}"
    );
    assert!(
        !story_2025.contains("ohio-senate-2026"),
        "a 2026 story is not filed under 2025: {story_2025}"
    );

    let undated = group(&html, "No dates yet");
    assert!(
        undated.contains("href=\"/timeline/no-events-yet\""),
        "a Timeline with no Events is listed, not dropped: {undated}"
    );

    assert!(
        !html.contains(">2024</h3>") && !html.contains(">2020</h3>"),
        "the edit year is never a heading"
    );
}

#[tokio::test]
async fn an_empty_collection_names_itself_and_offers_the_next_action() {
    let site = Site {
        pages: Vec::new(),
        collections: vec![collection("local-2026", "Local 2026")],
    };
    let html = render(&site, "/collection/local-2026").await;

    assert!(
        html.contains("No Timelines in Local 2026 yet."),
        "the empty state names the Collection it is about"
    );
    assert!(
        html.contains("href=\"/api/timeline\""),
        "the empty state offers the next action: publishing"
    );
    assert!(
        !html.contains("<ol"),
        "an empty Collection renders no empty list element"
    );
    assert!(
        !html.contains("<li class="),
        "an empty Collection renders no stray entry"
    );
}

/// `count` uncollected Timelines, updated one day apart, so Home's truncation
/// and the full list are both observable.
fn many_site(count: usize) -> Site {
    let pages = (0..count)
        .map(|index| {
            let id = format!("timeline-{index:02}");
            let day = format!("2026-01-{:02}T00:00:00Z", index + 1);
            page(
                &id,
                &format!("Timeline {index:02}"),
                None,
                Some("Demo Author"),
                &day,
                None,
                vec![event(&id, &format!("2026-01-{:02}", index + 1), "One fact")],
            )
        })
        .collect();
    Site {
        pages,
        collections: Vec::new(),
    }
}

#[tokio::test]
async fn home_points_at_a_full_list_and_the_full_list_shows_every_timeline() {
    let site = many_site(14);
    let home = render(&site, "/").await;

    assert!(
        home.contains("href=\"/timelines\""),
        "Home exposes the Timelines its twelve-entry preview leaves out"
    );
    assert!(
        home.contains("See all timelines"),
        "the destination is named for readers"
    );
    assert_eq!(
        home.matches("<li class=\"index__item\"").count(),
        12,
        "Home keeps its short 'recently updated' preview"
    );
    assert!(
        !home.contains("href=\"/timeline/timeline-00\"")
            && !home.contains("href=\"/timeline/timeline-01\""),
        "the preview really is the twelve most recent, so the link matters"
    );

    let all = render(&site, "/timelines").await;
    assert_eq!(
        all.matches("<li class=\"index__item\"").count(),
        14,
        "the full list hides nothing"
    );
    for page in &site.pages {
        assert!(
            all.contains(&format!("href=\"/timeline/{}\"", page.timeline.id)),
            "{} is listed at /timelines",
            page.timeline.id
        );
    }
    let newest = all.find("href=\"/timeline/timeline-13\"").expect("newest");
    let oldest = all.find("href=\"/timeline/timeline-00\"").expect("oldest");
    assert!(
        newest < oldest,
        "/timelines is sorted like Home: most recently updated first"
    );
    assert!(
        all.contains("href=\"/\"") && all.contains("Back to Home"),
        "the full list is not a dead end"
    );
}

/// The entry's meta line, where the facts sit.
fn meta(entry: &str) -> &str {
    let start = entry
        .find("<p class=\"index__meta\">")
        .expect("the entry has a meta line");
    let end = start + entry[start..].find("</p>").expect("the meta line closes") + "</p>".len();
    &entry[start..end]
}

#[tokio::test]
async fn lists_render_with_zero_one_and_many_timelines() {
    // Zero: no list element anywhere, and both surfaces still offer a way on.
    let empty = Site {
        pages: Vec::new(),
        collections: Vec::new(),
    };
    let home = render(&empty, "/").await;
    assert!(!home.contains("<ol"), "Home renders no empty list");
    assert!(!home.contains("<li class=\"index__item\""));
    assert!(
        home.contains("href=\"/timelines\""),
        "an empty site still exposes its full list"
    );

    let all = render(&empty, "/timelines").await;
    assert!(!all.contains("<ol"), "the full list renders no empty list");
    assert!(
        all.contains("No Timelines yet."),
        "the full list says it is empty"
    );
    assert!(
        all.contains("href=\"/api/timeline\""),
        "the empty full list offers the next action"
    );

    // One: exactly one entry, one meta line, no stray separators.
    let single = many_site(1);
    let home = render(&single, "/").await;
    assert_eq!(
        home.matches("<li class=\"index__item\"").count(),
        1,
        "one Timeline is one entry"
    );
    let only = entry(&home, "timeline-00");
    assert_eq!(
        only.matches("class=\"index__meta\"").count(),
        1,
        "one meta line per entry"
    );
    assert!(
        !only.contains(" \u{b7} ") && !only.contains(" | ") && !only.contains(", ,"),
        "no hand-written separator can go stray: {only}"
    );
    assert_eq!(
        meta(only).matches("<span").count(),
        3,
        "a dated, authored entry has span, byline and count: {}",
        meta(only)
    );
    assert_eq!(
        meta(only).matches("<time").count(),
        1,
        "and exactly one updated date: {}",
        meta(only)
    );

    // Many: every entry is complete, and the skeleton holds together.
    let many = render(&many_site(3), "/timelines").await;
    assert_eq!(
        many.matches("<li class=\"index__item\"").count(),
        3,
        "many Timelines are all listed"
    );
    assert_eq!(many.matches("class=\"index__meta\"").count(), 3);
    assert_eq!(many.matches("<ol class=\"index__list\">").count(), 1);

    // A Timeline with no Events still gets a complete entry, minus the span.
    let story = render(&story_site(), "/collection/elections-2026").await;
    let undated = entry(&story, "no-events-yet");
    assert_eq!(
        meta(undated).matches("<span").count(),
        2,
        "no invented span for a Timeline with no Events: {}",
        meta(undated)
    );
    assert!(
        meta(undated).contains("0 events"),
        "the count is still honest: {}",
        meta(undated)
    );
}

#[tokio::test]
async fn a_collection_reference_without_a_row_is_not_found_not_a_phantom_page() {
    // A Timeline may still carry a `collectionId` from before the picker
    // existed. With no COLLECTION row there is nothing to show, so the address
    // is a not-found rather than a page named after the id.
    let site = Site {
        pages: vec![page(
            "ghost",
            "Ghost Timeline",
            Some("deleted-collection"),
            None,
            "2026-01-01T00:00:00Z",
            None,
            vec![event("ghost", "2026-01-01", "A fact")],
        )],
        collections: Vec::new(),
    };

    let html = render(&site, "/collection/deleted-collection").await;
    assert!(html.contains("Collection not found"), "{html}");
    assert!(html.contains("Page not found"), "{html}");
    assert!(
        !html.contains("<h1 class=\"index__heading\">deleted-collection</h1>"),
        "the id never becomes a collection title: {html}"
    );
    assert!(
        site.collection_ids().is_empty(),
        "a dangling reference does not mint a static route"
    );
}

#[tokio::test]
async fn a_nested_collection_shows_its_parent_and_offers_owner_actions() {
    let mut child = collection("senate-races", "Senate Races");
    child.parent_collection_id = Some("elections-2026".to_owned());
    let site = Site {
        pages: list_site().pages,
        collections: vec![collection("elections-2026", "Elections 2026"), child],
    };

    let html = render(&site, "/collection/senate-races").await;
    assert!(
        html.contains("part of")
            && html.contains("href=\"/collection/elections-2026\"")
            && html.contains("Elections 2026"),
        "a nested collection says what it is part of: {html}"
    );

    // Owner actions ship hidden; the API decides who sees them.
    assert!(html.contains("data-owner-collection"), "{html}");
    assert!(
        html.contains("data-collection-id=\"senate-races\""),
        "the panel knows which collection it manages"
    );
    assert!(
        html.contains("class=\"manage\"") && html.contains(" hidden"),
        "the panel is hidden until the access check passes"
    );
    assert!(
        html.contains("action=\"/api/collections/senate-races\""),
        "rename, nest and delete post to the collection"
    );
    assert!(
        html.contains("action=\"/api/collections/senate-races/membership\""),
        "membership has its own form"
    );
    assert!(
        html.contains("name=\"parentCollectionId\""),
        "a nest picker"
    );
    assert!(
        !html.contains("<option value=\"senate-races\""),
        "a collection is not offered as its own parent: {html}"
    );
    assert!(
        html.contains("<option value=\"elections-2026\""),
        "its current parent is offered"
    );
    assert!(
        html.contains("data-confirm=\"Delete the empty collection"),
        "delete asks before it acts"
    );

    // Home lists root collections only; children live under their parent.
    let home = render(&site, "/").await;
    assert!(home.contains("Elections 2026"));
    assert!(
        !home.contains("Senate Races"),
        "a nested collection is reached through its parent, not Home: {home}"
    );
}
