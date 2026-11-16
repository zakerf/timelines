//! Turning a submitted payload into a stored Suggestion, and applying an
//! accepted EDIT or APPEND to the Timeline's Events.

use data::Timestamp;
use data::model::{AppendPayload, EditPayload, Event, Significance, SuggestionKind, Timeline};

use super::error::SuggestionError;
use super::request::PayloadRequest;
use crate::publish::{self, EventDraft, FieldError, PublishDraft, is_exact_date, is_http_source};

pub(super) fn parse_kind(value: &str) -> Result<SuggestionKind, SuggestionError> {
    match value.trim().to_ascii_uppercase().as_str() {
        "APPEND" => Ok(SuggestionKind::Append),
        "EDIT" => Ok(SuggestionKind::Edit),
        other => Err(SuggestionError::InvalidRequest(format!(
            "kind must be APPEND or EDIT, got {other:?}"
        ))),
    }
}

pub(super) fn validate_append(payload: &PayloadRequest) -> Result<AppendPayload, SuggestionError> {
    let date = payload.date.clone().unwrap_or_default();
    let title = payload.title.clone().unwrap_or_default();
    let description = payload.description.clone().unwrap_or_default();
    let sources = clean_sources(payload.sources.as_deref().unwrap_or_default());
    let mut errors = Vec::new();

    if !is_exact_date(&date) {
        errors.push(field(
            "payload.date",
            "exact date as YYYY-MM-DD is required",
        ));
    }
    if title.trim().is_empty() {
        errors.push(field("payload.title", "event title is required"));
    }
    if !sources.iter().any(|source| is_http_source(source)) {
        errors.push(field(
            "payload.sources",
            "at least one http(s) source URL is required",
        ));
    }
    let significance = parse_significance(
        payload.significance.as_deref(),
        "payload.significance",
        &mut errors,
    );

    if !errors.is_empty() {
        return Err(SuggestionError::Checklist(errors));
    }

    Ok(AppendPayload {
        date: date.trim().to_owned(),
        title: title.trim().to_owned(),
        description: description.trim().to_owned(),
        sources,
        significance,
    })
}

pub(super) fn validate_edit(payload: &PayloadRequest) -> Result<EditPayload, SuggestionError> {
    let mut errors = Vec::new();

    if payload.title.is_none()
        && payload.description.is_none()
        && payload.date.is_none()
        && payload.sources.is_none()
        && payload.significance.is_none()
    {
        errors.push(field("payload", "at least one proposed field is required"));
    }
    if let Some(date) = &payload.date
        && !is_exact_date(date)
    {
        errors.push(field(
            "payload.date",
            "exact date as YYYY-MM-DD is required",
        ));
    }
    if let Some(title) = &payload.title
        && title.trim().is_empty()
    {
        errors.push(field("payload.title", "event title cannot be blank"));
    }
    if let Some(sources) = &payload.sources
        && !sources.iter().any(|source| is_http_source(source))
    {
        errors.push(field(
            "payload.sources",
            "at least one http(s) source URL is required",
        ));
    }
    let significance = if payload.significance.is_some() {
        Some(parse_significance(
            payload.significance.as_deref(),
            "payload.significance",
            &mut errors,
        ))
    } else {
        None
    };

    if !errors.is_empty() {
        return Err(SuggestionError::Checklist(errors));
    }

    Ok(EditPayload {
        title: payload.title.as_ref().map(|title| title.trim().to_owned()),
        description: payload.description.clone(),
        date: payload.date.as_ref().map(|date| date.trim().to_owned()),
        sources: payload.sources.as_deref().map(clean_sources),
        significance,
    })
}

/// Reduce a prefilled form submission to the fields that actually differ from
/// the stored Event. The form starts at the Event's current values, so equality
/// means "leave unchanged"; a blank submitted over a non-blank value is a
/// proposed clearing and survives the diff.
pub(super) fn diff_against(current: &Event, submitted: &PayloadRequest) -> PayloadRequest {
    PayloadRequest {
        date: changed_text(submitted.date.as_deref(), &current.date),
        title: changed_text(submitted.title.as_deref(), &current.title),
        description: changed_text(submitted.description.as_deref(), &current.description),
        sources: submitted.sources.as_deref().and_then(|sources| {
            let cleaned = clean_sources(sources);
            (cleaned != current.sources).then_some(cleaned)
        }),
        significance: submitted.significance.as_deref().and_then(|value| {
            let value = value.trim();
            (!value.is_empty() && value != current.significance.as_str()).then(|| value.to_owned())
        }),
    }
}

/// `Some(value)` only when trimming it leaves something different from `current`.
fn changed_text(submitted: Option<&str>, current: &str) -> Option<String> {
    let submitted = submitted?;
    (submitted.trim() != current.trim()).then(|| submitted.to_owned())
}

/// True when a diffed edit proposes nothing at all.
pub(super) fn proposes_nothing(payload: &PayloadRequest) -> bool {
    payload.title.is_none()
        && payload.description.is_none()
        && payload.date.is_none()
        && payload.sources.is_none()
        && payload.significance.is_none()
}

/// The Event's current values in the request shape: what the form starts at.
pub(super) fn prefilled(event: &Event) -> PayloadRequest {
    PayloadRequest {
        date: Some(event.date.clone()),
        title: Some(event.title.clone()),
        description: Some(event.description.clone()),
        sources: Some(event.sources.clone()),
        significance: Some(event.significance.as_str().to_owned()),
    }
}

fn parse_significance(
    value: Option<&str>,
    field_name: &'static str,
    errors: &mut Vec<FieldError>,
) -> Significance {
    match value.map(str::trim) {
        None | Some("") => Significance::Regular,
        Some(value) => match value.parse() {
            Ok(significance) => significance,
            Err(_) => {
                errors.push(field(
                    field_name,
                    "significance must be minor, regular, or crucial",
                ));
                Significance::Regular
            }
        },
    }
}

fn clean_sources(sources: &[String]) -> Vec<String> {
    sources
        .iter()
        .map(|source| source.trim().to_owned())
        .filter(|source| !source.is_empty())
        .collect()
}

pub(super) fn field(name: &str, message: &str) -> FieldError {
    FieldError {
        field: name.to_owned(),
        message: message.to_owned(),
    }
}

/// Apply an accepted EDIT to the stored Event. `lastEditedBy` attributes the
/// change to the Suggestion's author; the approving owner is recorded on the
/// Suggestion itself.
pub(super) fn apply_edit(
    event: &Event,
    edit: &EditPayload,
    editor: &str,
    suggestion_id: &str,
) -> Event {
    let mut updated = event.clone();
    if let Some(date) = &edit.date {
        updated.date = date.clone();
        updated.timestamp =
            Timestamp::from_date(&updated.date).expect("validate_edit checked the exact date");
    }
    if let Some(title) = &edit.title {
        updated.title = title.clone();
    }
    if let Some(description) = &edit.description {
        updated.description = description.trim().to_owned();
    }
    if let Some(sources) = &edit.sources {
        updated.sources = sources.clone();
    }
    if let Some(significance) = edit.significance {
        updated.significance = significance;
    }
    updated.last_edited_by = Some(editor.to_owned());
    updated.last_edit_suggestion_id = Some(suggestion_id.to_owned());
    updated
}

/// An approved APPEND becomes a normal EVENT row, built by the publish
/// checklist's own constructor so the shapes cannot drift.
pub(super) fn append_event(
    timeline: &Timeline,
    append: &AppendPayload,
    now: &Timestamp,
) -> Result<Event, SuggestionError> {
    let draft = PublishDraft {
        title: timeline.title.clone(),
        description: timeline.description.clone(),
        collection_id: timeline.collection_id.clone(),
        events: vec![EventDraft {
            date: append.date.clone(),
            title: append.title.clone(),
            description: append.description.clone(),
            sources: append.sources.clone(),
            significance: Some(append.significance.as_str().to_owned()),
        }],
    };
    let event_id = publish::generated_id(&append.title);
    let built = publish::build(
        &draft,
        &timeline.owner_id,
        timeline.id.clone(),
        vec![event_id],
        now.clone(),
    )
    .map_err(SuggestionError::Checklist)?;
    built
        .events
        .into_iter()
        .next()
        .ok_or_else(|| SuggestionError::Internal("checklist built no event".to_owned()))
}

/// Re-run the publish checklist against the Timeline as the EDIT would leave
/// it. This is the gate PRD §8 calls "the back door is closed too".
pub(super) fn checklist_draft(timeline: &Timeline, events: &[Event]) -> PublishDraft {
    PublishDraft {
        title: timeline.title.clone(),
        description: timeline.description.clone(),
        collection_id: timeline.collection_id.clone(),
        events: events
            .iter()
            .map(|event| EventDraft {
                date: event.date.clone(),
                title: event.title.clone(),
                description: event.description.clone(),
                sources: event.sources.clone(),
                significance: Some(event.significance.as_str().to_owned()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::item::DynamoItem;

    const NOW: &str = "2026-11-08T12:34:56.789Z";
    const SOURCE: &str = "https://example.org/source";

    fn now() -> Timestamp {
        Timestamp::parse(NOW).expect("test timestamp")
    }

    fn append_payload() -> PayloadRequest {
        PayloadRequest {
            date: Some("2026-02-04".to_owned()),
            title: Some("Candidate filing deadline".to_owned()),
            description: Some("Petitions were due.".to_owned()),
            sources: Some(vec![SOURCE.to_owned()]),
            significance: None,
        }
    }

    fn sample_event() -> Event {
        Event {
            timeline_id: "t1".to_owned(),
            event_id: "e1".to_owned(),
            timestamp: Timestamp::from_date("2026-02-04").unwrap(),
            date: "2026-02-04".to_owned(),
            title: "Old title".to_owned(),
            description: "Old description".to_owned(),
            sources: vec![SOURCE.to_owned()],
            significance: Significance::Regular,
            upvotes: 3,
            downvotes: 1,
            report_count: 0,
            flagged: false,
            created_at: now(),
            created_by: "owner".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        }
    }

    fn sample_timeline() -> Timeline {
        Timeline {
            id: "t1".to_owned(),
            title: "A Timeline".to_owned(),
            description: "Deck".to_owned(),
            owner_id: "owner".to_owned(),
            collection_id: Some("c1".to_owned()),
            event_count: 1,
            upvotes: 0,
            downvotes: 0,
            created_at: now(),
            updated_at: now(),
            report_count: 0,
            flagged: false,
            published: true,
            cover_image: None,
        }
    }

    fn fields(error: SuggestionError) -> Vec<String> {
        match error {
            SuggestionError::Checklist(errors) => {
                errors.into_iter().map(|error| error.field).collect()
            }
            other => panic!("expected a checklist refusal, got {other:?}"),
        }
    }

    #[test]
    fn append_keeps_the_full_proposed_event() {
        let append = validate_append(&append_payload()).expect("valid append");
        assert_eq!(append.date, "2026-02-04");
        assert_eq!(append.title, "Candidate filing deadline");
        assert_eq!(append.sources, [SOURCE]);
        assert_eq!(append.significance, Significance::Regular);
    }

    #[test]
    fn append_requires_a_date_title_and_http_source() {
        let payload = PayloadRequest {
            date: Some("February 4".to_owned()),
            title: Some("  ".to_owned()),
            sources: Some(vec!["not a url".to_owned()]),
            ..append_payload()
        };
        assert_eq!(
            fields(validate_append(&payload).expect_err("invalid append")),
            ["payload.date", "payload.title", "payload.sources"]
        );
    }

    #[test]
    fn append_rejects_an_unknown_significance() {
        let payload = PayloadRequest {
            significance: Some("shouting".to_owned()),
            ..append_payload()
        };
        assert_eq!(
            fields(validate_append(&payload).expect_err("invalid significance")),
            ["payload.significance"]
        );
    }

    #[test]
    fn edit_keeps_only_the_proposed_fields() {
        let payload = PayloadRequest {
            title: Some("  Better title ".to_owned()),
            ..PayloadRequest::default()
        };
        let edit = validate_edit(&payload).expect("valid edit");
        assert_eq!(edit.title.as_deref(), Some("Better title"));
        assert_eq!(edit.description, None);
        assert_eq!(edit.date, None);
        assert_eq!(edit.sources, None);
        assert_eq!(edit.significance, None);
    }

    #[test]
    fn edit_requires_at_least_one_field() {
        assert_eq!(
            fields(validate_edit(&PayloadRequest::default()).expect_err("empty edit")),
            ["payload"]
        );
    }

    #[test]
    fn edit_validates_only_the_fields_it_proposes() {
        let payload = PayloadRequest {
            date: Some("2026-02-30".to_owned()),
            title: Some(String::new()),
            sources: Some(vec!["javascript:alert(1)".to_owned()]),
            ..PayloadRequest::default()
        };
        assert_eq!(
            fields(validate_edit(&payload).expect_err("invalid edit")),
            ["payload.date", "payload.title", "payload.sources"]
        );
    }

    #[test]
    fn an_accepted_edit_applies_in_place_and_stamps_attribution() {
        let event = sample_event();
        let edit = EditPayload {
            title: Some("New title".to_owned()),
            description: Some(" New description ".to_owned()),
            ..EditPayload::default()
        };
        let updated = apply_edit(&event, &edit, "author-2", "s1");

        assert_eq!(updated.title, "New title");
        assert_eq!(updated.description, "New description");
        assert_eq!(updated.timestamp, event.timestamp);
        assert_eq!(updated.upvotes, event.upvotes);
        assert_eq!(updated.created_by, event.created_by);
        assert_eq!(updated.last_edited_by.as_deref(), Some("author-2"));
        assert_eq!(updated.last_edit_suggestion_id.as_deref(), Some("s1"));
    }

    #[test]
    fn a_date_edit_moves_the_event_key() {
        let event = sample_event();
        let edit = EditPayload {
            date: Some("2026-05-05".to_owned()),
            ..EditPayload::default()
        };
        let updated = apply_edit(&event, &edit, "author-2", "s1");
        assert_eq!(updated.date, "2026-05-05");
        assert_eq!(
            updated.timestamp.as_str(),
            "2026-05-05T00:00:00.000Z",
            "date-only events get midnight UTC"
        );
        assert_ne!(updated.sk(), event.sk());
    }

    #[test]
    fn the_edit_checklist_rejects_a_timeline_with_a_bad_event() {
        let timeline = sample_timeline();
        let mut bad = sample_event();
        bad.sources = vec!["not a url".to_owned()];

        let errors = publish::validate(&checklist_draft(&timeline, &[bad]))
            .expect_err("the checklist catches the pre-existing bad event");
        assert_eq!(errors[0].field, "events[0].sources");
    }

    #[test]
    fn the_edit_checklist_accepts_a_valid_timeline() {
        let timeline = sample_timeline();
        assert_eq!(
            publish::validate(&checklist_draft(&timeline, &[sample_event()])),
            Ok(())
        );
    }

    #[test]
    fn kinds_are_case_insensitive_and_unknown_kinds_are_refused() {
        assert_eq!(parse_kind("append").unwrap(), SuggestionKind::Append);
        assert_eq!(parse_kind(" Edit ").unwrap(), SuggestionKind::Edit);
        assert!(parse_kind("delete").is_err());
    }

    fn prefilled(event: &Event) -> PayloadRequest {
        PayloadRequest {
            date: Some(event.date.clone()),
            title: Some(event.title.clone()),
            description: Some(event.description.clone()),
            sources: Some(event.sources.clone()),
            significance: Some(event.significance.as_str().to_owned()),
        }
    }

    #[test]
    fn an_untouched_prefilled_form_proposes_nothing() {
        let event = sample_event();
        let diff = diff_against(&event, &prefilled(&event));
        assert!(proposes_nothing(&diff), "nothing changed: {diff:?}");
    }

    #[test]
    fn the_diff_keeps_only_the_field_the_reader_corrected() {
        let event = sample_event();
        let submitted = PayloadRequest {
            title: Some(" Better title ".to_owned()),
            ..prefilled(&event)
        };
        let diff = diff_against(&event, &submitted);
        assert_eq!(diff.title.as_deref(), Some(" Better title "));
        assert_eq!(diff.description, None);
        assert_eq!(diff.date, None);
        assert_eq!(diff.sources, None);
        assert_eq!(diff.significance, None);
        assert!(!proposes_nothing(&diff));
    }

    #[test]
    fn clearing_a_field_is_a_proposed_change() {
        let event = sample_event();
        let submitted = PayloadRequest {
            description: Some(String::new()),
            ..prefilled(&event)
        };
        let diff = diff_against(&event, &submitted);
        assert_eq!(
            diff.description.as_deref(),
            Some(""),
            "a cleared description is a change, not 'unchanged'"
        );
    }

    #[test]
    fn an_edited_source_list_is_diffed_as_a_whole() {
        let event = sample_event();
        let submitted = PayloadRequest {
            sources: Some(vec![
                SOURCE.to_owned(),
                "https://example.org/second".to_owned(),
            ]),
            ..prefilled(&event)
        };
        let diff = diff_against(&event, &submitted);
        assert_eq!(
            diff.sources,
            Some(vec![
                SOURCE.to_owned(),
                "https://example.org/second".to_owned()
            ])
        );
    }

    #[test]
    fn significance_is_unchanged_when_it_matches() {
        let event = sample_event();
        let submitted = PayloadRequest {
            significance: Some(" regular ".to_owned()),
            ..prefilled(&event)
        };
        assert_eq!(diff_against(&event, &submitted).significance, None);

        let crucial = PayloadRequest {
            significance: Some("crucial".to_owned()),
            ..prefilled(&event)
        };
        let diff = diff_against(&event, &crucial);
        assert_eq!(diff.significance.as_deref(), Some("crucial"));
    }

    #[test]
    fn a_prefilled_submission_with_one_correction_validates() {
        let event = sample_event();
        let submitted = PayloadRequest {
            date: Some("2026-02-05".to_owned()),
            ..prefilled(&event)
        };
        let edit = validate_edit(&diff_against(&event, &submitted)).expect("a one-field edit");
        assert_eq!(edit.date.as_deref(), Some("2026-02-05"));
        assert_eq!(edit.title, None);
    }
}
