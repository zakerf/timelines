//! The owner's moderation queue as a page, and the before/after comparison a
//! reviewer needs before approving an edit.
//!
//! The queue is a human surface over the same store calls the JSON endpoint
//! uses: it lists every pending Suggestion across the owner's Timelines, shows
//! the submitter, the proposed change and the Event as it is now, and posts the
//! existing approve/reject endpoints as plain forms.

use data::model::{Event, Suggestion, SuggestionPayload};
use renderer::shell::{FORM_CSS, NOTICE_CSS, PageDocument};
use renderer::{human_date, updated_label};

use crate::http::escape_html;

/// One row of the queue: the Suggestion plus what a reviewer needs to judge it.
pub(super) struct QueueEntry {
    pub(super) suggestion: Suggestion,
    /// The Timeline's title, so the reviewer knows which story this is.
    pub(super) timeline_title: String,
    /// The submitter's display name, or a neutral phrase when the row is gone.
    pub(super) submitter: String,
    /// The Event an edit proposes to change, as it is right now.
    pub(super) target: Option<Event>,
    /// The Timeline owner, to build the approve/reject form actions.
    pub(super) timeline_id: String,
}

/// The queue page. `flash` is the one-line outcome of the last review action.
pub(super) fn queue_page(entries: &[QueueEntry], flash: Option<&str>) -> String {
    let flash = flash
        .map(|message| {
            format!(
                "<p class=\"form-error-summary\" role=\"status\">{}</p>",
                escape_html(message)
            )
        })
        .unwrap_or_default();

    let body = if entries.is_empty() {
        "<p class=\"empty\">Nothing to review right now. Pending suggestions from \
         readers appear here.</p>"
            .to_owned()
    } else {
        entries.iter().map(entry_html).collect::<String>()
    };

    let page = format!(
        r#"<main class="queue">
  <p class="eyebrow">Moderation</p>
  <h1>Pending suggestions</h1>
  <p class="lede">Readers propose corrections and missing Events. Approving applies the change and re-renders the affected pages; rejecting needs a note for the submitter.</p>
  {flash}
  <p class="help">{count} pending.</p>
  {body}
</main>"#,
        count = entries.len(),
    );

    PageDocument::new("Pending suggestions", &page)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .stylesheet(QUEUE_CSS)
        .noindex()
        .render()
}

/// The queue's own narrow styling: one card per Suggestion, a before/after
/// table, and the two review forms.
const QUEUE_CSS: &str = r#"
.queue { max-width: 52rem; }
.queue__item {
  margin: 1.75rem 0 0;
  padding: 1.25rem 1.35rem;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: #fff;
}
.queue__kind {
  margin: 0 0 0.35rem;
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--accent);
}
.queue__title { margin: 0 0 0.4rem; font-family: var(--serif); font-size: 1.35rem; }
.queue__meta { margin: 0 0 0.9rem; color: var(--muted); font-size: 0.85rem; }
.queue__meta a { color: var(--muted); }
.queue__compare { width: 100%; border-collapse: collapse; margin: 0.5rem 0 1rem; font-size: 0.92rem; }
.queue__compare th, .queue__compare td {
  padding: 0.4rem 0.55rem;
  border-bottom: 1px solid var(--line);
  text-align: left;
  vertical-align: top;
}
.queue__compare th { width: 8rem; color: var(--muted); font-weight: 600; }
.queue__compare .before { color: var(--muted); text-decoration: line-through; }
.queue__compare .after { color: var(--ink); font-weight: 600; }
.queue__form { margin: 1rem 0 0; padding-top: 0.9rem; border-top: 1px solid var(--line); }
.queue__form label { margin-top: 0; }
.queue__actions { display: flex; flex-wrap: wrap; gap: 1rem; align-items: baseline; margin-top: 1rem; }
.queue__actions button { margin-top: 0; }
.queue__actions .reject button { background: #fff; color: var(--accent); border: 1px solid var(--accent); }
.empty { margin: 1.5rem 0 0; color: var(--muted); font-style: italic; }
"#;

fn entry_html(entry: &QueueEntry) -> String {
    let suggestion = &entry.suggestion;
    let timeline_href = format!("/timeline/{}", escape_html(&entry.timeline_id));
    let submitted = updated_label(suggestion.submitted_at.as_str())
        .trim_start_matches("Updated ")
        .to_owned();
    let (kind, title, comparison) = match &suggestion.payload {
        SuggestionPayload::Append(append) => (
            "Proposed new event",
            append.title.clone(),
            append_comparison(append),
        ),
        SuggestionPayload::Edit(edit) => {
            let title = entry
                .target
                .as_ref()
                .map(|event| event.title.clone())
                .or_else(|| edit.title.clone())
                .unwrap_or_else(|| "An event".to_owned());
            (
                "Proposed edit",
                title,
                edit_comparison(entry.target.as_ref(), edit),
            )
        }
    };

    let approve_action = format!(
        "/api/timeline/{}/suggestions/{}/approve",
        escape_html(&entry.timeline_id),
        escape_html(&suggestion.suggestion_id)
    );
    let reject_action = format!(
        "/api/timeline/{}/suggestions/{}/reject",
        escape_html(&entry.timeline_id),
        escape_html(&suggestion.suggestion_id)
    );

    format!(
        r#"<article class="queue__item">
  <p class="queue__kind">{kind}</p>
  <h2 class="queue__title">{title}</h2>
  <p class="queue__meta">
    <a href="{timeline_href}">{timeline}</a> &middot;
    proposed by {submitter} &middot; {submitted}
  </p>
  {comparison}
  <div class="queue__actions">
    <form class="queue__form approve" method="post" action="{approve_action}">
      <label for="note-{id}-approve">Note to the submitter (optional)</label>
      <input id="note-{id}-approve" name="note" />
      <button type="submit">Approve and apply</button>
    </form>
    <form class="queue__form reject" method="post" action="{reject_action}">
      <label for="note-{id}-reject">Reason for rejecting (required)</label>
      <input id="note-{id}-reject" name="note" required />
      <button type="submit">Reject with this note</button>
    </form>
  </div>
</article>"#,
        kind = kind,
        title = escape_html(&title),
        timeline_href = timeline_href,
        timeline = escape_html(&entry.timeline_title),
        submitter = escape_html(&entry.submitter),
        submitted = escape_html(&submitted),
        comparison = comparison,
        approve_action = approve_action,
        reject_action = reject_action,
        id = escape_html(&suggestion.suggestion_id),
    )
}

fn edit_comparison(target: Option<&Event>, edit: &data::model::EditPayload) -> String {
    let mut rows = String::new();
    let mut push = |field: &str, before: String, after: String| {
        rows.push_str(&format!(
            "<tr><th scope=\"row\">{field}</th>\
             <td class=\"before\">{before}</td>\
             <td class=\"after\">{after}</td></tr>",
            before = escape_html(&before),
            after = escape_html(&after),
        ));
    };

    let current = |picked: fn(&Event) -> String| target.map(picked).unwrap_or_default();
    if let Some(date) = &edit.date {
        push(
            "Date",
            human_date(&current(|event| event.date.clone())),
            human_date(date),
        );
    }
    if let Some(title) = &edit.title {
        push("Title", current(|event| event.title.clone()), title.clone());
    }
    if let Some(description) = &edit.description {
        push(
            "Description",
            current(|event| event.description.clone()),
            if description.trim().is_empty() {
                "(removed)".to_owned()
            } else {
                description.clone()
            },
        );
    }
    if let Some(sources) = &edit.sources {
        push(
            "Sources",
            current(|event| event.sources.join(", ")),
            sources.join(", "),
        );
    }
    if let Some(significance) = edit.significance {
        push(
            "Significance",
            current(|event| event.significance.to_string()),
            significance.to_string(),
        );
    }

    if rows.is_empty() {
        return "<p class=\"help\">This suggestion proposes no change.</p>".to_owned();
    }
    format!(
        "<table class=\"queue__compare\">\
         <thead><tr><th scope=\"col\">Field</th><th scope=\"col\">Now</th>\
         <th scope=\"col\">Proposed</th></tr></thead>\
         <tbody>{rows}</tbody></table>"
    )
}

fn append_comparison(append: &data::model::AppendPayload) -> String {
    format!(
        "<table class=\"queue__compare\">\
         <tbody>\
         <tr><th scope=\"row\">Date</th><td class=\"after\">{date}</td></tr>\
         <tr><th scope=\"row\">Title</th><td class=\"after\">{title}</td></tr>\
         <tr><th scope=\"row\">Description</th><td class=\"after\">{description}</td></tr>\
         <tr><th scope=\"row\">Sources</th><td class=\"after\">{sources}</td></tr>\
         <tr><th scope=\"row\">Significance</th><td class=\"after\">{significance}</td></tr>\
         </tbody></table>",
        date = escape_html(&human_date(&append.date)),
        title = escape_html(&append.title),
        description = escape_html(&append.description),
        sources = escape_html(&append.sources.join(", ")),
        significance = escape_html(append.significance.as_str()),
    )
}

/// A refusal the reviewer can act on (already reviewed, not yours, store down).
pub(super) fn queue_notice_page(heading: &str, message: &str) -> String {
    let body = format!(
        "<main><h1>{}</h1><p>{}</p>\
         <p><a href=\"/api/suggestions/queue\">Back to the queue</a></p></main>",
        escape_html(heading),
        escape_html(message),
    );
    PageDocument::new(heading, &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::Timestamp;
    use data::model::{AppendPayload, EditPayload, Significance};

    fn now() -> Timestamp {
        Timestamp::parse("2026-11-08T12:34:56.789Z").expect("timestamp")
    }

    fn suggestion(payload: SuggestionPayload) -> Suggestion {
        Suggestion {
            suggestion_id: "s1".to_owned(),
            timeline_id: "t1".to_owned(),
            owner_id: Some("owner".to_owned()),
            state: data::model::SuggestionState::Pending,
            kind: match payload {
                SuggestionPayload::Append(_) => data::model::SuggestionKind::Append,
                SuggestionPayload::Edit(_) => data::model::SuggestionKind::Edit,
            },
            target_event_id: Some("e1".to_owned()),
            payload,
            submitted_by: "reader".to_owned(),
            submitted_at: now(),
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            report_count: 0,
            flagged: false,
        }
    }

    fn target_event() -> Event {
        Event {
            timeline_id: "t1".to_owned(),
            event_id: "e1".to_owned(),
            timestamp: Timestamp::from_date("2026-02-04").expect("date"),
            date: "2026-02-04".to_owned(),
            title: "Old title".to_owned(),
            description: "Old description".to_owned(),
            sources: vec!["https://example.org/old".to_owned()],
            significance: Significance::Regular,
            upvotes: 0,
            downvotes: 0,
            report_count: 0,
            flagged: false,
            created_at: now(),
            created_by: "owner".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        }
    }

    fn entry(payload: SuggestionPayload) -> QueueEntry {
        QueueEntry {
            suggestion: suggestion(payload),
            timeline_title: "Ohio Senate 2026".to_owned(),
            submitter: "Sam Reader".to_owned(),
            target: Some(target_event()),
            timeline_id: "t1".to_owned(),
        }
    }

    #[test]
    fn an_edit_shows_the_value_now_against_the_proposal() {
        let html = queue_page(
            &[entry(SuggestionPayload::Edit(EditPayload {
                title: Some("<b>New</b> title".to_owned()),
                ..EditPayload::default()
            }))],
            None,
        );
        assert!(html.contains("Proposed edit"));
        assert!(html.contains("Old title"), "the current value is shown");
        assert!(html.contains("&lt;b&gt;New&lt;/b&gt; title"), "escaped");
        assert!(html.contains("Sam Reader"), "the submitter is named");
        assert!(html.contains("Ohio Senate 2026"));
        assert!(html.contains("action=\"/api/timeline/t1/suggestions/s1/approve\""));
        assert!(html.contains("action=\"/api/timeline/t1/suggestions/s1/reject\""));
        assert!(
            html.contains("Reason for rejecting (required)"),
            "a rejection needs a note"
        );
        assert!(html.contains("method=\"post\""));
    }

    #[test]
    fn an_append_shows_the_proposed_event() {
        let html = queue_page(
            &[entry(SuggestionPayload::Append(AppendPayload {
                date: "2026-03-01".to_owned(),
                title: "A missing fact".to_owned(),
                description: "It happened.".to_owned(),
                sources: vec!["https://example.org/new".to_owned()],
                significance: Significance::Crucial,
            }))],
            None,
        );
        assert!(html.contains("Proposed new event"));
        assert!(html.contains("A missing fact"));
        assert!(html.contains("1 March 2026"), "dates read as prose");
        assert!(html.contains("crucial"));
    }

    #[test]
    fn an_empty_queue_says_so_and_the_page_is_one_document() {
        let html = queue_page(&[], None);
        assert!(html.contains("Nothing to review right now"));
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert_eq!(html.matches("<style").count(), 1);
        assert!(html.contains("noindex"));
    }

    #[test]
    fn a_flash_message_is_escaped() {
        let html = queue_page(&[], Some("<script>alert(1)</script>"));
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
