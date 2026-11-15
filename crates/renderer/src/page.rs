//! View data and the deterministic no-cover gradient.
//!
//! A [`Site`] is the whole set of Timelines a render run has loaded. Pages are
//! plain owned data: the renderer reads DynamoDB first, then renders from
//! memory, so no reactive resource and no async component is needed during the
//! static render.

use data::model::{Collection, Event, Timeline};

/// Everything a render run loaded from DynamoDB.
#[derive(Clone, Debug, Default)]
pub struct Site {
    /// One page per Timeline. Ordered by [`Timeline::id`] for deterministic output.
    pub pages: Vec<TimelinePage>,
    /// Every COLLECTION row, ordered by id. Collection pages and Home's entry
    /// points read from here.
    pub collections: Vec<Collection>,
}

impl Site {
    /// Ids in stable order; the static-route prerender list comes from here.
    pub fn timeline_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .pages
            .iter()
            .map(|page| page.timeline.id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Collection ids that need a `/collection/:id` page: every COLLECTION row
    /// plus any id a Timeline references (so a publish pointing at a
    /// not-yet-created collection still gets a page).
    pub fn collection_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .collections
            .iter()
            .map(|collection| collection.id.clone())
            .chain(
                self.pages
                    .iter()
                    .filter_map(|page| page.timeline.collection_id.clone()),
            )
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Top-level collections, in title order. Home's collection entry points.
    pub fn root_collections(&self) -> Vec<Collection> {
        let mut collections: Vec<Collection> = self
            .collections
            .iter()
            .filter(|collection| collection.parent_collection_id.is_none())
            .cloned()
            .collect();
        collections.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        collections
    }

    /// Other Timelines in the same collection, newest update first, never
    /// `exclude_id` itself: the read page's "More in this collection" block.
    pub fn siblings(&self, collection_id: &str, exclude_id: &str) -> Vec<TimelinePage> {
        let mut pages: Vec<TimelinePage> = self
            .pages
            .iter()
            .filter(|page| page.timeline.id != exclude_id)
            .filter(|page| page.timeline.collection_id.as_deref() == Some(collection_id))
            .cloned()
            .collect();
        pages.sort_by(|left, right| {
            right
                .timeline
                .updated_at
                .cmp(&left.timeline.updated_at)
                .then_with(|| left.timeline.id.cmp(&right.timeline.id))
        });
        pages
    }

    /// Direct child collections of `collection_id`, in title order.
    pub fn child_collections(&self, collection_id: &str) -> Vec<Collection> {
        let mut collections: Vec<Collection> = self
            .collections
            .iter()
            .filter(|collection| collection.parent_collection_id.as_deref() == Some(collection_id))
            .cloned()
            .collect();
        collections.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        collections
    }
}

/// A Collection page's direct Timelines, grouped by the year of their last
/// update, newest year first.
#[derive(Clone, Debug)]
pub struct TimelineGroup {
    pub year: String,
    pub pages: Vec<TimelinePage>,
}

impl TimelineGroup {
    /// Human-facing heading: `2026` for a `2026-11-08T...` timestamp.
    pub fn label(&self) -> &str {
        &self.year
    }
}

/// Group a collection's direct Timelines by update year, newest first, then by
/// id. Deterministic for identical input.
pub fn group_timelines_by_year<I>(pages: I) -> Vec<TimelineGroup>
where
    I: IntoIterator<Item = TimelinePage>,
{
    let mut pages: Vec<TimelinePage> = pages.into_iter().collect();
    pages.sort_by(|left, right| {
        right
            .timeline
            .updated_at
            .cmp(&left.timeline.updated_at)
            .then_with(|| left.timeline.id.cmp(&right.timeline.id))
    });

    let mut groups: Vec<TimelineGroup> = Vec::new();
    for page in pages {
        let year = page.timeline.updated_at.as_str()[..4].to_owned();
        match groups.last_mut() {
            Some(group) if group.year == year => group.pages.push(page),
            _ => groups.push(TimelineGroup {
                year,
                pages: vec![page],
            }),
        }
    }
    groups
}

/// A Timeline plus its ordered Events and the denormalized byline/eyebrow.
#[derive(Clone, Debug)]
pub struct TimelinePage {
    pub timeline: Timeline,
    /// Chronological: `SK` is `EVENT#<timestamp>#<id>` and DynamoDB queries
    /// return it in ascending order.
    pub events: Vec<Event>,
    /// `ACCOUNT#<ownerId>.displayName`, when the owner row exists.
    pub owner_name: Option<String>,
    /// `COLLECTION#<collectionId>.title`, when the Timeline is collected.
    pub collection_title: Option<String>,
}

/// Two deterministic hues derived from the Timeline id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gradient {
    pub hue: u16,
    pub accent: u16,
}

impl Gradient {
    /// Inline custom properties consumed by `.hero--gradient` in the stylesheet.
    pub fn style(self) -> String {
        format!(
            "--gradient-from:hsl({} 42% 21%);--gradient-to:hsl({} 52% 10%);",
            self.hue, self.accent
        )
    }
}

/// FNV-1a over the Timeline id. Stable across runs, platforms, and hosts, so
/// the same Timeline always gets the same hero even with no stored cover.
pub fn gradient_for(timeline_id: &str) -> Gradient {
    const FNV_OFFSET: u32 = 2_166_136_261;
    const FNV_PRIME: u32 = 16_777_619;

    let hash = timeline_id.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    });
    let hue = (hash % 360) as u16;
    let accent = ((u32::from(hue) + 35 + (hash >> 9) % 70) % 360) as u16;
    Gradient { hue, accent }
}

/// Reader-facing label for a Source chip: the host, without the scheme or path.
pub fn source_label(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_start_matches("www.");
    host.to_owned()
}

/// One chip label per Source, in the Event's own order: the host, numbered
/// `host (1)`, `host (2)` when the same host is cited more than once so two
/// chips stay distinguishable.
pub fn source_labels(sources: &[String]) -> Vec<String> {
    let hosts: Vec<String> = sources.iter().map(|url| source_label(url)).collect();
    hosts
        .iter()
        .enumerate()
        .map(|(index, host)| {
            let repeated = hosts.iter().filter(|other| *other == host).count() > 1;
            if repeated {
                let ordinal = hosts[..index].iter().filter(|other| *other == host).count() + 1;
                format!("{host} ({ordinal})")
            } else {
                host.clone()
            }
        })
        .collect()
}

/// The name a reader sees as the author. A Timeline whose ACCOUNT row is
/// missing gets [`UNKNOWN_AUTHOR`]; a raw account id is never shown.
pub fn author_name(owner_name: Option<&str>) -> &str {
    match owner_name.map(str::trim) {
        Some(name) if !name.is_empty() => name,
        _ => UNKNOWN_AUTHOR,
    }
}

/// How a Timeline is bylined when the owner's ACCOUNT row is missing.
pub const UNKNOWN_AUTHOR: &str = "the editors";

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A date as prose: `2026-02-04` and `2026-02-04T09:00:00Z` both read
/// "4 February 2026". Anything that is not an ISO date naming a real calendar
/// day is returned unchanged, so an odd value still renders instead of
/// disappearing. No date library: the renderer only ever sees `YYYY-MM-DD…`.
pub fn human_date(value: &str) -> String {
    match parse_iso_date(value) {
        Some((year, month, day)) => {
            format!("{day} {} {year}", MONTH_NAMES[month - 1])
        }
        None => value.to_owned(),
    }
}

/// The hero's freshness line: "Updated 8 November 2026".
pub fn updated_label(updated_at: &str) -> String {
    format!("Updated {}", human_date(updated_at))
}

/// "1 event" / "3 events".
pub fn event_count_label(count: usize) -> String {
    if count == 1 {
        "1 event".to_owned()
    } else {
        format!("{count} events")
    }
}

/// `YYYY-MM-DD` (optionally followed by a time) -> (year, month, day), but only
/// when every component is a digit and the day exists in that month.
fn parse_iso_date(value: &str) -> Option<(i32, usize, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() < 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    if bytes.len() > 10 && !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let year: i32 = value[..4].parse().ok()?;
    let month: usize = value[5..7].parse().ok()?;
    let day: u32 = value[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

fn days_in_month(year: i32, month: usize) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap_year(year) => 29,
        _ => 28,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Only `http(s)` Sources become links. The publish checklist guarantees this,
/// but rendering refuses to turn any other scheme into an `href`.
pub fn is_http_source(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::Timestamp;

    #[test]
    fn dates_read_as_prose() {
        assert_eq!(human_date("2026-02-04"), "4 February 2026");
        assert_eq!(human_date("2026-11-08"), "8 November 2026");
        assert_eq!(human_date("2026-02-04T09:00:00Z"), "4 February 2026");
        assert_eq!(human_date("2024-02-29T23:59:59.000Z"), "29 February 2024");
    }

    #[test]
    fn odd_dates_fall_back_to_the_raw_value() {
        assert_eq!(human_date("not a date"), "not a date");
        assert_eq!(human_date("2026-02-30"), "2026-02-30", "no such day");
        assert_eq!(human_date("2023-02-29"), "2023-02-29", "not a leap year");
        assert_eq!(human_date("2026-13-01"), "2026-13-01", "no such month");
        assert_eq!(human_date(""), "");
    }

    #[test]
    fn the_updated_label_uses_the_same_formatter() {
        assert_eq!(
            updated_label("2026-11-08T21:32:00.000Z"),
            "Updated 8 November 2026"
        );
        assert_eq!(updated_label("whenever"), "Updated whenever");
    }

    #[test]
    fn event_counts_are_pluralized() {
        assert_eq!(event_count_label(0), "0 events");
        assert_eq!(event_count_label(1), "1 event");
        assert_eq!(event_count_label(3), "3 events");
    }

    #[test]
    fn a_missing_owner_name_never_becomes_an_id() {
        assert_eq!(author_name(Some("Demo Author")), "Demo Author");
        assert_eq!(author_name(None), "the editors");
        assert_eq!(author_name(Some("   ")), "the editors");
    }

    #[test]
    fn same_host_sources_are_numbered() {
        let labels = source_labels(&[
            "https://www.ohiosos.gov/elections/".to_owned(),
            "https://example.org/primary".to_owned(),
            "https://www.ohiosos.gov/results/".to_owned(),
        ]);
        assert_eq!(
            labels,
            ["ohiosos.gov (1)", "example.org", "ohiosos.gov (2)"]
        );
    }

    #[test]
    fn siblings_are_the_other_timelines_in_the_same_collection() {
        let mut current = page("current", "2026-11-08T00:00:00Z");
        current.timeline.collection_id = Some("collection".to_owned());
        let mut older = page("older", "2025-01-01T00:00:00Z");
        older.timeline.collection_id = Some("collection".to_owned());
        let mut newer = page("newer", "2026-12-01T00:00:00Z");
        newer.timeline.collection_id = Some("collection".to_owned());
        let mut elsewhere = page("elsewhere", "2026-06-01T00:00:00Z");
        elsewhere.timeline.collection_id = None;
        let site = Site {
            pages: vec![current.clone(), older, newer, elsewhere],
            collections: Vec::new(),
        };

        assert_eq!(
            site.siblings("collection", "current")
                .iter()
                .map(|page| page.timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["newer", "older"],
            "newest first, and never the Timeline being read"
        );
        assert!(
            site.siblings("no-such-collection", "current").is_empty(),
            "an unknown collection has no siblings"
        );
    }

    #[test]
    fn source_label_is_the_host() {
        assert_eq!(
            source_label("https://www.ohiosos.gov/elections/candidates/"),
            "ohiosos.gov"
        );
        assert_eq!(source_label("http://example.com"), "example.com");
        assert_eq!(source_label("not a url"), "not a url");
    }

    #[test]
    fn only_http_sources_are_links() {
        assert!(is_http_source("https://example.com"));
        assert!(is_http_source("http://example.com"));
        assert!(!is_http_source("javascript:alert(1)"));
        assert!(!is_http_source("data:text/html,hi"));
    }

    fn page(id: &str, updated_at: &str) -> TimelinePage {
        let timestamp = Timestamp::parse(updated_at).expect("timestamp");
        TimelinePage {
            timeline: Timeline {
                id: id.to_owned(),
                title: id.to_owned(),
                description: String::new(),
                owner_id: "owner".to_owned(),
                collection_id: Some("collection".to_owned()),
                event_count: 0,
                upvotes: 0,
                downvotes: 0,
                created_at: timestamp.clone(),
                updated_at: timestamp,
                report_count: 0,
                flagged: false,
                cover_image: None,
            },
            events: Vec::new(),
            owner_name: None,
            collection_title: None,
        }
    }

    #[test]
    fn timelines_group_by_update_year_newest_first() {
        let groups = group_timelines_by_year([
            page("old", "2025-03-01T00:00:00Z"),
            page("new", "2026-11-08T00:00:00Z"),
            page("same-year", "2026-01-01T00:00:00Z"),
        ]);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label(), "2026");
        assert_eq!(
            groups[0]
                .pages
                .iter()
                .map(|page| page.timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["new", "same-year"],
            "newest update first inside a year"
        );
        assert_eq!(groups[1].label(), "2025");
    }

    #[test]
    fn collection_ids_include_rows_and_referenced_ids() {
        let mut site = Site {
            pages: vec![page("t", "2026-01-01T00:00:00Z")],
            collections: Vec::new(),
        };
        site.collections.push(Collection {
            id: "a".to_owned(),
            title: "A".to_owned(),
            description: String::new(),
            parent_collection_id: None,
            created_at: Timestamp::parse("2026-01-01T00:00:00Z").expect("timestamp"),
            created_by: "owner".to_owned(),
        });
        site.collections.push(Collection {
            id: "nested".to_owned(),
            title: "Nested".to_owned(),
            description: String::new(),
            parent_collection_id: Some("a".to_owned()),
            created_at: Timestamp::parse("2026-01-01T00:00:00Z").expect("timestamp"),
            created_by: "owner".to_owned(),
        });

        assert_eq!(
            site.collection_ids(),
            ["a", "collection", "nested"],
            "row ids and referenced ids both get pages"
        );
        assert_eq!(
            site.root_collections()
                .iter()
                .map(|collection| collection.id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert_eq!(
            site.child_collections("a")
                .iter()
                .map(|collection| collection.id.as_str())
                .collect::<Vec<_>>(),
            ["nested"]
        );
    }
}
