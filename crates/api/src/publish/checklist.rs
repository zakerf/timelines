//! The publish checklist (PRD §8) and construction of the items a publish
//! writes.
//!
//! The checklist is hard gates only: a non-empty Timeline title, at least one
//! Event, and every Event with an exact date, a non-empty title, and at least
//! one syntactically valid `http(s)` Source. Every gate is collected in one
//! pass so the caller can return a per-field error list, not just the first
//! failure.

use data::Timestamp;
use data::model::{Emphasis, Event, Timeline};
use serde::Serialize;
use url::Url;

/// One field-level checklist failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// One Event as the publish form or JSON supplies it, before validation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventDraft {
    pub date: String,
    pub title: String,
    pub description: String,
    pub sources: Vec<String>,
    pub emphasis: Option<String>,
}

impl EventDraft {
    pub fn new(
        date: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        sources: Vec<String>,
    ) -> Self {
        Self {
            date: date.into(),
            title: title.into(),
            description: description.into(),
            sources,
            emphasis: None,
        }
    }
}

/// A whole publish request before validation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishDraft {
    pub title: String,
    pub description: String,
    pub collection_id: Option<String>,
    pub events: Vec<EventDraft>,
}

/// The rows a valid publish writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTimeline {
    pub timeline: Timeline,
    pub events: Vec<Event>,
}

/// Run every hard gate. `Ok(())` means the draft may be published.
pub fn validate(draft: &PublishDraft) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();

    if draft.title.trim().is_empty() {
        errors.push(FieldError::new("title", "timeline title is required"));
    }

    if draft.events.is_empty() {
        errors.push(FieldError::new("events", "at least one event is required"));
    }

    for (index, event) in draft.events.iter().enumerate() {
        if !is_exact_date(&event.date) {
            errors.push(FieldError::new(
                format!("events[{index}].date"),
                "exact date as YYYY-MM-DD is required",
            ));
        }
        if event.title.trim().is_empty() {
            errors.push(FieldError::new(
                format!("events[{index}].title"),
                "event title is required",
            ));
        }
        if !event.sources.iter().any(|source| is_http_source(source)) {
            errors.push(FieldError::new(
                format!("events[{index}].sources"),
                "at least one http(s) source URL is required",
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate and build the TIMELINE/EVENT rows. The caller supplies the ids and
/// the single timestamp applied to every row, so key generation stays in the
/// API layer and this stays a pure function.
pub fn build(
    draft: &PublishDraft,
    owner_id: &str,
    timeline_id: String,
    event_ids: Vec<String>,
    now: Timestamp,
) -> Result<NewTimeline, Vec<FieldError>> {
    validate(draft)?;

    let events: Vec<Event> = draft
        .events
        .iter()
        .zip(event_ids)
        .map(|(draft_event, event_id)| Event {
            timeline_id: timeline_id.clone(),
            event_id,
            timestamp: Timestamp::from_date(&draft_event.date)
                .expect("validate() rejected every non-exact date"),
            date: draft_event.date.trim().to_owned(),
            title: draft_event.title.trim().to_owned(),
            description: draft_event.description.trim().to_owned(),
            sources: draft_event
                .sources
                .iter()
                .map(|source| source.trim().to_owned())
                .filter(|source| !source.is_empty())
                .collect(),
            emphasis: emphasis_for(draft_event.emphasis.as_deref()),
            upvotes: 0,
            downvotes: 0,
            report_count: 0,
            flagged: false,
            created_at: now.clone(),
            created_by: owner_id.to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        })
        .collect();

    let timeline = Timeline {
        id: timeline_id,
        title: draft.title.trim().to_owned(),
        description: draft.description.trim().to_owned(),
        owner_id: owner_id.to_owned(),
        collection_id: draft
            .collection_id
            .as_ref()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty()),
        event_count: i64::try_from(events.len()).expect("an event list fits in i64"),
        upvotes: 0,
        downvotes: 0,
        created_at: now.clone(),
        updated_at: now,
        report_count: 0,
        flagged: false,
        cover_image: None,
    };

    Ok(NewTimeline { timeline, events })
}

/// An exact reader-facing date in `YYYY-MM-DD` form (fuzzy dates are fog).
pub fn is_exact_date(value: &str) -> bool {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit)
        && Timestamp::from_date(trimmed).is_ok()
}

/// A Source is a link only when it parses as `http(s)` with a host.
pub fn is_http_source(source: &str) -> bool {
    match Url::parse(source.trim()) {
        Ok(url) => matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        Err(_) => false,
    }
}

/// A path-safe slug for a generated Timeline/Event id. The renderer's static
/// route writer accepts only ASCII alphanumerics, `-`, and `_`.
pub fn slugify(value: &str) -> String {
    const MAX: usize = 48;

    let mut slug = String::new();
    let mut last_dash = false;
    for character in value.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= MAX {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "timeline".to_owned()
    } else {
        slug.to_owned()
    }
}

fn emphasis_for(value: Option<&str>) -> Emphasis {
    match value {
        Some("featured") => Emphasis::Featured,
        _ => Emphasis::Standard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(url: &str) -> Vec<String> {
        vec![url.to_owned()]
    }

    fn valid_draft() -> PublishDraft {
        PublishDraft {
            title: "Ohio Senate 2026".to_owned(),
            description: "Key dates.".to_owned(),
            collection_id: Some("elections-2026".to_owned()),
            events: vec![
                EventDraft::new(
                    "2026-02-04",
                    "Candidate filing deadline",
                    "Petitions were due.",
                    source("https://www.ohiosos.gov/elections/candidates/"),
                ),
                EventDraft::new(
                    "2026-05-05",
                    "Primary election day",
                    "Nominees were decided.",
                    source("http://example.org/primary"),
                ),
            ],
        }
    }

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|error| error.field.as_str()).collect()
    }

    #[test]
    fn a_complete_draft_passes_every_gate() {
        assert_eq!(validate(&valid_draft()), Ok(()));
    }

    #[test]
    fn an_empty_title_is_a_title_error() {
        let draft = PublishDraft {
            title: "   ".to_owned(),
            ..valid_draft()
        };
        let errors = validate(&draft).expect_err("blank title fails");
        assert_eq!(fields(&errors), ["title"]);
    }

    #[test]
    fn zero_events_fails_with_an_events_error() {
        let draft = PublishDraft {
            events: Vec::new(),
            ..valid_draft()
        };
        let errors = validate(&draft).expect_err("no events fails");
        assert_eq!(fields(&errors), ["events"]);
    }

    #[test]
    fn every_event_needs_an_exact_date() {
        let mut draft = valid_draft();
        draft.events[0].date = "February 4".to_owned();
        draft.events[1].date = "2026-2-4".to_owned();

        let errors = validate(&draft).expect_err("inexact dates fail");
        assert_eq!(fields(&errors), ["events[0].date", "events[1].date"]);
    }

    #[test]
    fn every_event_needs_a_non_empty_title() {
        let mut draft = valid_draft();
        draft.events[1].title = "  ".to_owned();

        let errors = validate(&draft).expect_err("blank event title fails");
        assert_eq!(fields(&errors), ["events[1].title"]);
    }

    #[test]
    fn every_event_needs_at_least_one_valid_http_source() {
        for sources in [
            Vec::new(),
            vec!["not a url".to_owned()],
            vec!["ftp://example.org/file".to_owned()],
            vec!["javascript:alert(1)".to_owned()],
            vec!["https://".to_owned()],
        ] {
            let mut draft = valid_draft();
            draft.events[0].sources = sources;

            let errors = validate(&draft).expect_err("invalid source fails");
            assert_eq!(fields(&errors), ["events[0].sources"]);
        }
    }

    #[test]
    fn one_valid_source_is_enough_even_beside_invalid_ones() {
        let mut draft = valid_draft();
        draft.events[0].sources = vec!["not a url".to_owned(), "https://example.org/ok".to_owned()];
        assert_eq!(validate(&draft), Ok(()));
    }

    #[test]
    fn all_failing_gates_are_reported_together() {
        let draft = PublishDraft {
            title: String::new(),
            description: String::new(),
            collection_id: None,
            events: vec![EventDraft::new(
                "someday",
                " ",
                "",
                vec!["ftp://example.org".to_owned()],
            )],
        };

        let errors = validate(&draft).expect_err("every gate fails");
        assert_eq!(
            fields(&errors),
            [
                "title",
                "events[0].date",
                "events[0].title",
                "events[0].sources"
            ]
        );
    }

    #[test]
    fn build_stamps_owner_count_and_utc_timestamps() {
        let now = Timestamp::parse("2026-11-08T12:00:00Z").expect("timestamp");
        let draft = valid_draft();
        let built = build(
            &draft,
            "account-1",
            "ohio-senate-2026-abcd12".to_owned(),
            vec![
                "filing-deadline-aaaaaa".to_owned(),
                "primary-day-bbbbbb".to_owned(),
            ],
            now.clone(),
        )
        .expect("valid draft builds");

        assert_eq!(built.timeline.id, "ohio-senate-2026-abcd12");
        assert_eq!(built.timeline.title, "Ohio Senate 2026");
        assert_eq!(built.timeline.owner_id, "account-1");
        assert_eq!(
            built.timeline.collection_id.as_deref(),
            Some("elections-2026")
        );
        assert_eq!(built.timeline.event_count, 2);
        assert_eq!(built.timeline.created_at, now);
        assert_eq!(built.timeline.updated_at, now);
        assert_eq!(built.timeline.upvotes, 0);
        assert_eq!(built.timeline.downvotes, 0);
        assert_eq!(built.timeline.cover_image, None);
        assert_eq!(built.events.len(), 2);
        assert_eq!(built.events[0].timeline_id, "ohio-senate-2026-abcd12");
        assert_eq!(built.events[0].created_by, "account-1");
        assert_eq!(built.events[0].created_at, now);
        assert_eq!(built.events[0].date, "2026-02-04");
        assert_eq!(
            built.events[0].timestamp.as_str(),
            "2026-02-04T00:00:00.000Z",
            "a date-only event gets midnight UTC"
        );
        assert_eq!(built.events[1].emphasis, Emphasis::Standard);
    }

    #[test]
    fn build_refuses_an_invalid_draft() {
        let draft = PublishDraft {
            events: Vec::new(),
            ..valid_draft()
        };
        let errors = build(
            &draft,
            "account-1",
            "t".to_owned(),
            Vec::new(),
            Timestamp::now(),
        )
        .expect_err("invalid draft does not build");
        assert_eq!(fields(&errors), ["events"]);
    }

    #[test]
    fn slugs_are_path_safe_and_never_empty() {
        assert_eq!(slugify("Ohio Senate 2026"), "ohio-senate-2026");
        assert_eq!(slugify("  --Hello, World!--  "), "hello-world");
        assert_eq!(slugify("日本語"), "timeline");
        assert_eq!(slugify(""), "timeline");
        assert!(slugify(&"a".repeat(100)).len() <= 48);
    }

    #[test]
    fn exact_dates_are_strict_yyyy_mm_dd() {
        assert!(is_exact_date("2026-02-04"));
        assert!(is_exact_date("2024-02-29"), "leap day is exact");
        assert!(!is_exact_date("2026-2-4"));
        assert!(!is_exact_date("2026-02-30"));
        assert!(!is_exact_date("2026-02-04T00:00:00Z"));
        assert!(!is_exact_date("someday"));
    }
}
