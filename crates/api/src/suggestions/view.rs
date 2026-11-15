//! JSON shapes for Suggestion responses and the script-free edit form page.

use data::model::{Suggestion, SuggestionPayload};
use renderer::shell::{FORM_CSS, NOTICE_CSS, PageDocument};

use crate::http::escape_html;

/// The owner's pending queue (GSI1) or a Timeline's public pending list.
pub(super) fn suggestions_json(suggestions: &[Suggestion]) -> serde_json::Value {
    serde_json::Value::Array(suggestions.iter().map(suggestion_json).collect())
}

pub(super) fn suggestion_json(suggestion: &Suggestion) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert(
        "id".to_owned(),
        serde_json::Value::String(suggestion.suggestion_id.clone()),
    );
    value.insert(
        "timelineId".to_owned(),
        serde_json::Value::String(suggestion.timeline_id.clone()),
    );
    value.insert(
        "state".to_owned(),
        serde_json::Value::String(suggestion.state.as_str().to_owned()),
    );
    value.insert(
        "kind".to_owned(),
        serde_json::Value::String(suggestion.kind.as_str().to_owned()),
    );
    if let Some(target) = &suggestion.target_event_id {
        value.insert(
            "targetEventId".to_owned(),
            serde_json::Value::String(target.clone()),
        );
    }
    value.insert("payload".to_owned(), payload_json(&suggestion.payload));
    value.insert(
        "submittedBy".to_owned(),
        serde_json::Value::String(suggestion.submitted_by.clone()),
    );
    value.insert(
        "submittedAt".to_owned(),
        serde_json::Value::String(suggestion.submitted_at.as_str().to_owned()),
    );
    if let Some(reviewer) = &suggestion.reviewed_by {
        value.insert(
            "reviewedBy".to_owned(),
            serde_json::Value::String(reviewer.clone()),
        );
    }
    if let Some(reviewed_at) = &suggestion.reviewed_at {
        value.insert(
            "reviewedAt".to_owned(),
            serde_json::Value::String(reviewed_at.as_str().to_owned()),
        );
    }
    if let Some(note) = &suggestion.review_note {
        value.insert(
            "reviewNote".to_owned(),
            serde_json::Value::String(note.clone()),
        );
    }
    serde_json::Value::Object(value)
}

fn payload_json(payload: &SuggestionPayload) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    match payload {
        SuggestionPayload::Append(append) => {
            value.insert(
                "date".to_owned(),
                serde_json::Value::String(append.date.clone()),
            );
            value.insert(
                "title".to_owned(),
                serde_json::Value::String(append.title.clone()),
            );
            value.insert(
                "description".to_owned(),
                serde_json::Value::String(append.description.clone()),
            );
            value.insert(
                "sources".to_owned(),
                serde_json::Value::Array(
                    append
                        .sources
                        .iter()
                        .map(|source| serde_json::Value::String(source.clone()))
                        .collect(),
                ),
            );
            value.insert(
                "significance".to_owned(),
                serde_json::Value::String(append.significance.as_str().to_owned()),
            );
        }
        SuggestionPayload::Edit(edit) => {
            if let Some(date) = &edit.date {
                value.insert("date".to_owned(), serde_json::Value::String(date.clone()));
            }
            if let Some(title) = &edit.title {
                value.insert("title".to_owned(), serde_json::Value::String(title.clone()));
            }
            if let Some(description) = &edit.description {
                value.insert(
                    "description".to_owned(),
                    serde_json::Value::String(description.clone()),
                );
            }
            if let Some(sources) = &edit.sources {
                value.insert(
                    "sources".to_owned(),
                    serde_json::Value::Array(
                        sources
                            .iter()
                            .map(|source| serde_json::Value::String(source.clone()))
                            .collect(),
                    ),
                );
            }
            if let Some(significance) = edit.significance {
                value.insert(
                    "significance".to_owned(),
                    serde_json::Value::String(significance.as_str().to_owned()),
                );
            }
        }
    }
    serde_json::Value::Object(value)
}

/// "Suggest an edit" form body. The placeholders are filled with the route's
/// Timeline and Event ids at render time; the document shell, tokens, and base
/// form styles come from [`renderer::shell`]. Plain form POST with a small
/// inline script for the repeatable source rows.
const SUGGEST_FORM_BODY: &str = r#"<main>
  <h1>Suggest an edit</h1>
  <p class="lede">Leave a field blank to keep it unchanged. The owner reviews your suggestion before it is applied.</p>
  <form method="post" action="/api/timeline/{timeline_id}/suggestions">
    <input type="hidden" name="kind" value="EDIT" />
    <input type="hidden" name="targetEventId" value="{event_id}" />

    <label for="date">Date</label>
    <input id="date" name="date" placeholder="YYYY-MM-DD" />

    <label for="title">Title</label>
    <input id="title" name="title" />

    <fieldset>
      <legend>Sources</legend>
      <div id="source-rows">
        <div class="source-row">
          <input name="sources" type="url" placeholder="https://source" />
          <button type="button" class="remove-source" hidden>Remove</button>
        </div>
      </div>
      <button type="button" class="add-source" id="add-source">Add another source</button>
      <p class="help">Leave every source blank to keep the current sources.</p>
    </fieldset>

    <label for="description">Description</label>
    <textarea id="description" name="description"></textarea>

    <fieldset>
      <legend>Significance</legend>
      <div class="significance">
        <label><input type="radio" name="significance" value="" checked /> Leave unchanged</label>
        <label><input type="radio" name="significance" value="minor" /> Minor</label>
        <label><input type="radio" name="significance" value="regular" /> Regular</label>
        <label><input type="radio" name="significance" value="crucial" /> Crucial</label>
      </div>
    </fieldset>

    <button type="submit">Submit suggestion</button>
  </form>
</main>
<script>
  (function () {
    var rows = document.getElementById("source-rows");
    var add = document.getElementById("add-source");

    function sync() {
      var all = rows.querySelectorAll(".source-row");
      all.forEach(function (row, index) {
        row.querySelector(".remove-source").hidden = all.length === 1;
        row.querySelector("input").placeholder =
          index === 0 ? "https://source" : "https://another-source";
      });
    }

    add.addEventListener("click", function () {
      var row = document.createElement("div");
      row.className = "source-row";
      row.innerHTML =
        '<input name="sources" type="url" placeholder="https://another-source" />' +
        '<button type="button" class="remove-source">Remove</button>';
      rows.appendChild(row);
      sync();
      row.querySelector("input").focus();
    });

    rows.addEventListener("click", function (event) {
      var button = event.target.closest(".remove-source");
      if (!button || rows.querySelectorAll(".source-row").length === 1) return;
      button.closest(".source-row").remove();
      sync();
    });

    sync();
  })();
</script>"#;

/// The suggest form as a complete document for the route's Timeline and Event.
pub(super) fn suggest_form_html(timeline_id: &str, event_id: &str) -> String {
    let body = SUGGEST_FORM_BODY
        .replace("{timeline_id}", &escape_html(timeline_id))
        .replace("{event_id}", &escape_html(event_id));
    PageDocument::new("Suggest an edit", &body)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::Timestamp;
    use data::model::{EditPayload, SuggestionKind, SuggestionState};

    #[test]
    fn suggestion_json_has_the_camel_case_contract() {
        let suggestion = Suggestion {
            suggestion_id: "s1".to_owned(),
            timeline_id: "t1".to_owned(),
            owner_id: Some("owner".to_owned()),
            state: SuggestionState::Pending,
            kind: SuggestionKind::Edit,
            target_event_id: Some("e1".to_owned()),
            payload: SuggestionPayload::Edit(EditPayload {
                title: Some("Better".to_owned()),
                ..EditPayload::default()
            }),
            submitted_by: "author".to_owned(),
            submitted_at: Timestamp::parse("2026-11-08T12:34:56.789Z").expect("test timestamp"),
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            report_count: 0,
            flagged: false,
        };
        let json = suggestion_json(&suggestion);
        assert_eq!(json["id"], "s1");
        assert_eq!(json["timelineId"], "t1");
        assert_eq!(json["state"], "PENDING");
        assert_eq!(json["kind"], "EDIT");
        assert_eq!(json["targetEventId"], "e1");
        assert_eq!(json["payload"]["title"], "Better");
        assert!(json["payload"].get("date").is_none());
    }

    #[test]
    fn the_form_page_renders_through_the_shared_shell() {
        let html = suggest_form_html("t1", "e1");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html lang=\"en\">"));
        assert!(html.trim_end().ends_with("</html>"));
        assert_eq!(html.matches("<style").count(), 1, "one stylesheet");
        assert_eq!(html.matches("--paper:").count(), 1, "one token block");
        assert!(html.contains("action=\"/api/timeline/t1/suggestions\""));
        assert!(html.contains("value=\"e1\""), "the Event is carried");
        // A form page carries the same site header as every other page and no
        // reader navigation: sign in stays usable, no identity is baked.
        assert!(
            html.contains("<header class=\"site-header\">"),
            "the site header"
        );
        assert!(
            html.contains("href=\"/api/auth/login?next=/\" data-sign-in>Sign in</a>"),
            "the anonymous account area"
        );
        assert!(
            !html.contains("Recently updated") && !html.contains("Back to all Timelines"),
            "no reader chrome on a form"
        );
    }

    #[test]
    fn the_form_page_escapes_route_ids() {
        let html = suggest_form_html("t\"><script>", "e1");
        assert!(!html.contains("<script>alert"));
        assert!(
            !html.contains("t\"><script>"),
            "escaped ids cannot break out of an attribute"
        );
        assert!(html.contains("&quot;"), "the quote is escaped");
    }
}
