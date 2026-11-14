//! JSON shapes for Suggestion responses and the script-free edit form page.

use data::model::{Suggestion, SuggestionPayload};

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

/// "Suggest an edit" form page. The placeholders are filled with the route's
/// Timeline and Event ids. Plain form POST with a small inline script for the
/// repeatable source rows.
pub(super) const SUGGEST_FORM_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Suggest an edit</title>
  <style>
    :root { color-scheme: light; --paper: #faf8f4; --ink: #17171a; --line: #ddd6c9; --accent: #a63a23; }
    * { box-sizing: border-box; }
    body { margin: 0; background: var(--paper); color: var(--ink); font-family: "Segoe UI", system-ui, sans-serif; }
    main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.35rem 5rem; }
    h1 { font-family: Georgia, serif; font-size: clamp(2rem, 5vw, 3rem); margin: 0 0 0.4rem; }
    p.lede { color: #5c5a60; margin: 0 0 2rem; }
    label { display: block; margin: 1.1rem 0 0.35rem; font-weight: 600; font-size: 0.9rem; }
    input, textarea { width: 100%; padding: 0.6rem 0.7rem; border: 1px solid var(--line); border-radius: 5px; background: #fff; font: inherit; }
    textarea { min-height: 6rem; }
    fieldset { margin: 1.1rem 0 0; padding: 0; border: 0; }
    legend { padding: 0; font-weight: 600; font-size: 0.9rem; }
    .source-row { display: flex; gap: 0.5rem; margin-top: 0.35rem; }
    .source-row input { flex: 1; }
    button.remove-source, button.add-source { margin-top: 0; padding: 0.45rem 0.8rem; font-size: 0.85rem; }
    button.add-source { margin-top: 0.55rem; background: #fff; color: var(--accent); border: 1px solid var(--line); }
    .significance { display: flex; flex-wrap: wrap; gap: 1.1rem; margin-top: 0.45rem; }
    .significance label { display: flex; align-items: center; gap: 0.4rem; margin: 0; font-weight: 400; }
    .significance input { width: auto; }
    .help { margin: 0.35rem 0 0; color: #5c5a60; font-size: 0.82rem; }
    button[type="submit"] { margin-top: 1.6rem; padding: 0.65rem 1.2rem; border: 0; border-radius: 5px; background: var(--accent); color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
  </style>
</head>
<body>
<main>
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
</script>
</body>
</html>
"#;

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
}
