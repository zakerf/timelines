//! The owner's report summary as a page: counts and flags, never a reporter.
//!
//! `GET /api/timeline/{id}/reports` keeps its JSON contract for clients and
//! serves this page to a browser, negotiated on `Accept` exactly like the
//! moderation queue. The page answers the only two questions an owner has -
//! what was reported, and how many accounts reported it - and repeats that
//! nothing is hidden or removed automatically.

use data::model::{Event, Suggestion, SuggestionPayload, Timeline};
use renderer::shell::{NOTICE_CSS, PageDocument};

use crate::http::escape_html;

/// The summary page for one Timeline.
pub(super) fn summary_page_html(
    timeline: &Timeline,
    events: &[Event],
    suggestions: &[Suggestion],
) -> String {
    let timeline_href = format!("/timeline/{}", escape_html(&timeline.id));
    let reported_events: Vec<&Event> = events
        .iter()
        .filter(|event| event.report_count > 0)
        .collect();
    let reported_suggestions: Vec<&Suggestion> = suggestions
        .iter()
        .filter(|suggestion| suggestion.report_count > 0)
        .collect();

    let total =
        usize::from(timeline.report_count > 0) + reported_events.len() + reported_suggestions.len();
    let body = if total == 0 {
        format!(
            r#"<p class="flags__empty">Nothing on <a href="{timeline_href}">{timeline_title}</a> has been reported.</p>"#,
            timeline_title = escape_html(&timeline.title),
        )
    } else {
        let mut rows = String::new();
        if timeline.report_count > 0 {
            rows.push_str(&format!(
                "<tr><th scope=\"row\"><a href=\"{href}\">This timeline</a></th>\
                 <td>{count}</td><td>{flag}</td></tr>",
                href = timeline_href,
                count = report_count_cell(timeline.report_count),
                flag = flag_cell(timeline.flagged),
            ));
        }
        for event in reported_events {
            rows.push_str(&format!(
                "<tr><th scope=\"row\"><a href=\"/timeline/{timeline_id}#event-{event_id}\">{title}</a></th>\
                 <td>{count}</td><td>{flag}</td></tr>",
                timeline_id = escape_html(&timeline.id),
                event_id = escape_html(&event.event_id),
                title = escape_html(&event.title),
                count = report_count_cell(event.report_count),
                flag = flag_cell(event.flagged),
            ));
        }
        for suggestion in reported_suggestions {
            rows.push_str(&format!(
                "<tr><th scope=\"row\"><a href=\"/api/suggestions/queue\">{title}</a></th>\
                 <td>{count}</td><td>{flag}</td></tr>",
                title = escape_html(&suggestion_label(suggestion)),
                count = report_count_cell(suggestion.report_count),
                flag = flag_cell(suggestion.flagged),
            ));
        }
        format!(
            "<table class=\"flags__table\">\
             <thead><tr><th scope=\"col\">Content</th><th scope=\"col\">Reports</th>\
             <th scope=\"col\">State</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    let page = format!(
        r#"<main class="flags">
  <p class="eyebrow">Moderation</p>
  <h1>Reports and flags</h1>
  <p class="lede">Readers can report a Timeline, an Event, or a pending suggestion.
  You see how many accounts reported each one and whether it reached three, which
  flags it. You never see who reported. Nothing is hidden or removed automatically:
  read the content and decide what to do.</p>
  <p class="flags__back"><a href="{timeline_href}">Back to {timeline_title}</a></p>
  {body}
</main>"#,
        timeline_href = timeline_href,
        timeline_title = escape_html(&timeline.title),
        body = body,
    );

    PageDocument::new("Reports and flags", &page)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FLAGS_CSS)
        .noindex()
        .render()
}

/// A refusal the owner sees as a page rather than a JSON body.
pub(super) fn summary_notice_page(heading: &str, message: &str) -> String {
    let body = format!(
        "<main><h1>{}</h1><p>{}</p>\
         <p><a href=\"/\">Back to Home</a></p></main>",
        escape_html(heading),
        escape_html(message),
    );
    PageDocument::new(heading, &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
}

fn report_count_cell(count: i64) -> String {
    if count == 1 {
        "1 report".to_owned()
    } else {
        format!("{count} reports")
    }
}

fn flag_cell(flagged: bool) -> String {
    if flagged {
        "<span class=\"flag flag--on\">Flagged</span>".to_owned()
    } else {
        "<span class=\"flag\">Below threshold</span>".to_owned()
    }
}

/// Enough of a Suggestion to recognise it in a list.
fn suggestion_label(suggestion: &Suggestion) -> String {
    match &suggestion.payload {
        SuggestionPayload::Append(append) => format!("Proposed new event: {}", append.title),
        SuggestionPayload::Edit(edit) => edit
            .title
            .clone()
            .map(|title| format!("Proposed edit: {title}"))
            .unwrap_or_else(|| "Proposed edit".to_owned()),
    }
}

const FLAGS_CSS: &str = r#"
.flags { max-width: 46rem; }
.flags__back { margin: 0.9rem 0 1.5rem; }
.flags__back a { color: var(--muted); font-size: 0.9rem; }
.flags__table { width: 100%; border-collapse: collapse; margin: 1rem 0 0; font-size: 0.95rem; }
.flags__table th, .flags__table td {
  padding: 0.55rem 0.6rem;
  border-bottom: 1px solid var(--line);
  text-align: left;
  vertical-align: top;
}
.flags__table thead th {
  font-size: 0.75rem;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--muted);
}
.flags__table tbody th { font-weight: 600; }
.flags__table tbody th a { color: var(--ink); }
.flag {
  display: inline-block;
  padding: 0.1rem 0.45rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  font-size: 0.75rem;
  color: var(--muted);
}
.flag--on {
  border-color: #9a3220;
  color: #9a3220;
  font-weight: 600;
}
.flags__empty { margin: 1.4rem 0 0; color: var(--muted); font-style: italic; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use data::Timestamp;
    use data::model::{AppendPayload, Significance};

    fn timestamp() -> Timestamp {
        Timestamp::parse("2026-11-08T12:34:56.789Z").expect("timestamp")
    }

    fn timeline() -> Timeline {
        Timeline {
            id: "t1".to_owned(),
            title: "Ohio Senate 2026".to_owned(),
            description: "The race.".to_owned(),
            owner_id: "owner".to_owned(),
            collection_id: None,
            event_count: 1,
            upvotes: 0,
            downvotes: 0,
            report_count: 3,
            flagged: true,
            created_at: timestamp(),
            updated_at: timestamp(),
            cover_image: None,
        }
    }

    fn event(title: &str, report_count: i64, flagged: bool) -> Event {
        Event {
            timeline_id: "t1".to_owned(),
            event_id: "e1".to_owned(),
            timestamp: Timestamp::from_date("2026-02-04").expect("date"),
            date: "2026-02-04".to_owned(),
            title: title.to_owned(),
            description: String::new(),
            sources: Vec::new(),
            significance: Significance::Regular,
            upvotes: 0,
            downvotes: 0,
            report_count,
            flagged,
            created_at: timestamp(),
            created_by: "owner".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        }
    }

    fn suggestion(report_count: i64) -> Suggestion {
        Suggestion {
            suggestion_id: "s1".to_owned(),
            timeline_id: "t1".to_owned(),
            owner_id: Some("owner".to_owned()),
            state: data::model::SuggestionState::Pending,
            kind: data::model::SuggestionKind::Append,
            target_event_id: None,
            payload: SuggestionPayload::Append(AppendPayload {
                date: "2026-03-01".to_owned(),
                title: "A missing fact".to_owned(),
                description: "It happened.".to_owned(),
                sources: vec!["https://example.org/new".to_owned()],
                significance: Significance::Regular,
            }),
            submitted_by: "reader".to_owned(),
            submitted_at: timestamp(),
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            report_count,
            flagged: report_count >= 3,
        }
    }

    #[test]
    fn the_summary_lists_every_reported_target_with_counts_and_flags() {
        let html = summary_page_html(
            &timeline(),
            &[event("Candidate filing deadline", 4, true)],
            &[suggestion(2)],
        );

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("Reports and flags"));
        assert!(html.contains("Ohio Senate 2026"), "the timeline is named");
        assert!(html.contains("Candidate filing deadline"));
        assert!(html.contains("3 reports"), "{html}");
        assert!(html.contains("4 reports"));
        assert!(html.contains("2 reports"));
        assert!(html.contains("Flagged"), "the threshold is shown");
        assert!(html.contains("Below threshold"));
        assert_eq!(
            html.matches("<style").count(),
            1,
            "one stylesheet through the shared shell"
        );
        assert!(html.contains("noindex"), "an owner page is not indexed");
    }

    #[test]
    fn only_reported_content_appears() {
        let html = summary_page_html(
            &Timeline {
                report_count: 0,
                flagged: false,
                ..timeline()
            },
            &[event("Nobody reported this", 0, false)],
            &[suggestion(0)],
        );

        assert!(html.contains("Nothing on"));
        assert!(!html.contains("Nobody reported this"), "{html}");
        assert!(!html.contains("Below threshold"), "{html}");
    }

    #[test]
    fn the_page_never_names_a_reporter() {
        let html = summary_page_html(&timeline(), &[event("A fact", 3, true)], &[suggestion(3)]);
        assert!(
            !html.contains("reader") && !html.contains("accountId") && !html.contains("ACCOUNT#"),
            "no reporter identity reaches the owner: {html}"
        );
        assert!(
            html.contains("You never see who reported"),
            "the promise is on the page"
        );
    }

    #[test]
    fn a_flag_is_not_signalled_by_colour_alone() {
        let html = summary_page_html(&timeline(), &[event("A fact", 3, true)], &[]);
        assert!(
            html.contains(">Flagged</span>"),
            "the word carries the state, not the colour: {html}"
        );
    }

    #[test]
    fn target_text_is_escaped() {
        let html = summary_page_html(
            &timeline(),
            &[event("<script>alert(1)</script>", 3, true)],
            &[],
        );
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }
}
