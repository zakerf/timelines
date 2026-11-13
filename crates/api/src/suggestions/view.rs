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
                "emphasis".to_owned(),
                serde_json::Value::String(append.emphasis.as_str().to_owned()),
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
            if let Some(emphasis) = edit.emphasis {
                value.insert(
                    "emphasis".to_owned(),
                    serde_json::Value::String(emphasis.as_str().to_owned()),
                );
            }
        }
    }
    serde_json::Value::Object(value)
}

/// "Suggest an edit" form page. The placeholders are filled with the route's
/// Timeline and Event ids. Plain form POST; no script dependency.
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
    .help { margin: 0.35rem 0 0; color: #5c5a60; font-size: 0.82rem; }
    button { margin-top: 1.6rem; padding: 0.65rem 1.2rem; border: 0; border-radius: 5px; background: var(--accent); color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
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

    <label for="sources">Sources</label>
    <input id="sources" name="sources" placeholder="https://source, https://source2" />

    <label for="description">Description</label>
    <textarea id="description" name="description"></textarea>

    <label for="emphasis">Emphasis (optional)</label>
    <input id="emphasis" name="emphasis" placeholder="standard or featured" />

    <button type="submit">Submit suggestion</button>
  </form>
</main>
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
