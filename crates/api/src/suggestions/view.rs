//! JSON shapes for Suggestion responses and the script-free edit form page.

use data::model::{Event, Suggestion, SuggestionPayload};
use renderer::shell::{FORM_CSS, NOTICE_CSS, PageDocument};

use super::request::PayloadRequest;
use crate::http::escape_html;
use crate::publish::FieldError;

/// The public pending list: the same contract plus the submitter's display
/// name, so a reader sees who proposed the change instead of an opaque id.
pub(super) fn pending_json(
    suggestions: &[Suggestion],
    names: &[(String, String)],
) -> serde_json::Value {
    serde_json::Value::Array(
        suggestions
            .iter()
            .map(|suggestion| {
                let mut value = suggestion_json(suggestion);
                if let Some((_, name)) = names
                    .iter()
                    .find(|(account, _)| account == &suggestion.submitted_by)
                    && let Some(object) = value.as_object_mut()
                {
                    object.insert(
                        "submittedByName".to_owned(),
                        serde_json::Value::String(name.clone()),
                    );
                }
                value
            })
            .collect(),
    )
}

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

/// "Suggest an edit" form body. The document shell, tokens, and base form
/// styles come from [`renderer::shell`]; the values and messages come from the
/// Event and the (possibly refused) submission.
///
/// Every field starts at the Event's current value, which is what "leave it as
/// it is" means here: the reader changes only the field they are correcting and
/// the router diffs the result against the stored Event.
fn suggest_form_body(
    timeline_id: &str,
    event: &Event,
    submitted: &PayloadRequest,
    errors: &[FieldError],
) -> String {
    let date = submitted.date.as_deref().unwrap_or(&event.date);
    let title = submitted.title.as_deref().unwrap_or(&event.title);
    let description = submitted
        .description
        .as_deref()
        .unwrap_or(&event.description);
    let sources = submitted
        .sources
        .clone()
        .unwrap_or_else(|| event.sources.clone());
    let significance = submitted
        .significance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(event.significance.as_str());

    let summary = (!errors.is_empty()).then(|| {
        let items = errors
            .iter()
            .map(|error| format!("<li>{}</li>", escape_html(&error.message)))
            .collect::<String>();
        format!(
            "<div class=\"form-error-summary\" role=\"alert\">\
             <p><strong>Your suggestion was not sent.</strong></p>\
             <ul>{items}</ul></div>"
        )
    });
    let summary = summary.unwrap_or_default();

    let source_rows = sources
        .iter()
        .map(|source| {
            format!(
                "<div class=\"source-row\">\
                 <input name=\"sources\" type=\"url\" value=\"{}\" placeholder=\"https://source\" />\
                 <button type=\"button\" class=\"remove-source\">Remove</button>\
                 </div>",
                escape_html(source)
            )
        })
        .collect::<String>();
    let source_rows = if source_rows.is_empty() {
        "<div class=\"source-row\">\
         <input name=\"sources\" type=\"url\" placeholder=\"https://source\" />\
         <button type=\"button\" class=\"remove-source\" hidden>Remove</button>\
         </div>"
            .to_owned()
    } else {
        source_rows
    };

    let radio = |value: &str, label: &str| {
        let checked = if significance == value {
            " checked"
        } else {
            ""
        };
        format!(
            "<label><input type=\"radio\" name=\"significance\" value=\"{value}\"{checked} /> {label}</label>"
        )
    };

    let timeline_href = format!(
        "/timeline/{}#event-{}",
        escape_html(timeline_id),
        escape_html(&event.event_id)
    );
    let form_action = format!("/api/timeline/{}/suggestions", escape_html(timeline_id));
    let event_id = escape_html(&event.event_id);

    format!(
        r#"<main>
  <p class="eyebrow">Suggest an edit</p>
  <h1>Suggest an edit</h1>
  <p class="lede">Each field below starts at this Event's current value. Change only what is wrong; anything you leave as it is stays exactly as it is. The Timeline's owner reviews your suggestion before it is applied.</p>
  {summary}
  <form method="post" action="{form_action}">
    <input type="hidden" name="kind" value="EDIT" />
    <input type="hidden" name="targetEventId" value="{event_id}" />

    {date_field}

    {title_field}

    {description_field}

    <fieldset>
      <legend>Sources</legend>
      <p class="help">These are the current sources. Leave them as they are to keep them unchanged; edit or add a row to propose a different set.</p>
      <div id="source-rows" data-source-rows>{source_rows}</div>
      <button type="button" class="add-source" id="add-source">Add another source</button>
    </fieldset>

    <fieldset>
      <legend>Significance</legend>
      <p class="help">Currently <strong>{current_significance_label}</strong>. Minor is background detail, Regular is part of the story, Crucial is a turning point. Leave it as it is to keep it unchanged.</p>
      <div class="significance">
        {radio_minor}
        {radio_regular}
        {radio_crucial}
      </div>
    </fieldset>

    <div class="form-actions">
      <button type="submit">Submit suggestion</button>
      <a class="form-cancel" href="{timeline_href}">Cancel and go back to the Event</a>
    </div>
  </form>
</main>
{script}"#,
        summary = summary,
        form_action = form_action,
        event_id = event_id,
        date_field = field_input(
            "date",
            "Date",
            &format!(
                "<input id=\"date\" name=\"date\" type=\"date\" value=\"{}\"{} />",
                escape_html(date),
                invalid_attribute(errors, "date")
            ),
            "The Event's date. Change it to correct it; leave it to keep it unchanged.",
            errors,
            "date",
        ),
        title_field = field_input(
            "title",
            "Title",
            &format!(
                "<input id=\"title\" name=\"title\" value=\"{}\"{} />",
                escape_html(title),
                invalid_attribute(errors, "title")
            ),
            "The headline for this Event. Leave it as it is to keep it unchanged.",
            errors,
            "title",
        ),
        description_field = field_input(
            "description",
            "Description",
            &format!(
                "<textarea id=\"description\" name=\"description\"{}>{}</textarea>",
                invalid_attribute(errors, "description"),
                escape_html(description)
            ),
            "What happened. Clear it to suggest removing the description; leave it to keep it unchanged.",
            errors,
            "description",
        ),
        current_significance_label = escape_html(significance_label(significance)),
        radio_minor = radio("minor", "Minor"),
        radio_regular = radio("regular", "Regular"),
        radio_crucial = radio("crucial", "Crucial"),
        source_rows = source_rows,
        timeline_href = timeline_href,
        script = SOURCE_ROWS_SCRIPT,
    )
}

/// The repeatable source rows: add a row, remove a row, never leave zero rows
/// when the current Event has sources. Plain DOM, no framework.
const SOURCE_ROWS_SCRIPT: &str = r#"<script>
  (function () {
    var rows = document.getElementById("source-rows");
    var add = document.getElementById("add-source");
    if (!rows || !add) return;

    function sync() {
      var all = rows.querySelectorAll(".source-row");
      all.forEach(function (row) {
        row.querySelector(".remove-source").hidden = all.length <= 1;
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
      if (!button || rows.querySelectorAll(".source-row").length <= 1) return;
      button.closest(".source-row").remove();
      sync();
    });

    sync();
  })();
</script>"#;

/// One labelled field: the control, the per-field state sentence, and the
/// inline message when this field is the one that failed.
fn field_input(
    id: &str,
    label: &str,
    control: &str,
    hint: &str,
    errors: &[FieldError],
    field: &str,
) -> String {
    let error = errors
        .iter()
        .find(|error| error.field == format!("payload.{field}"))
        .map(|error| {
            format!(
                "<p class=\"field-error\" id=\"{id}-error\">{}</p>",
                escape_html(&error.message)
            )
        })
        .unwrap_or_default();
    format!(
        "<div class=\"field\">\
         <label for=\"{id}\">{}</label>\
         {control}\
         <p class=\"hint\">{}</p>\
         {error}\
         </div>",
        escape_html(label),
        escape_html(hint)
    )
}

fn invalid_attribute(errors: &[FieldError], field: &str) -> String {
    let invalid = errors
        .iter()
        .any(|error| error.field == format!("payload.{field}"));
    if invalid {
        format!(" aria-invalid=\"true\" aria-describedby=\"{field}-error\"")
    } else {
        String::new()
    }
}

/// The empty "suggest a missing event" form. Same fields, validation and error
/// behaviour as an edit, but nothing to prefill.
fn append_form_body(
    timeline_id: &str,
    submitted: &PayloadRequest,
    errors: &[FieldError],
) -> String {
    let date = submitted.date.as_deref().unwrap_or_default();
    let title = submitted.title.as_deref().unwrap_or_default();
    let description = submitted.description.as_deref().unwrap_or_default();
    let sources: Vec<String> = submitted.sources.clone().unwrap_or_default();
    let significance = submitted
        .significance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("regular");

    let summary = (!errors.is_empty()).then(|| {
        let items = errors
            .iter()
            .map(|error| format!("<li>{}</li>", escape_html(&error.message)))
            .collect::<String>();
        format!(
            "<div class=\"form-error-summary\" role=\"alert\">\
             <p><strong>Your suggestion was not sent.</strong></p>\
             <ul>{items}</ul></div>"
        )
    });
    let summary = summary.unwrap_or_default();

    let source_rows = sources
        .iter()
        .map(|source| {
            format!(
                "<div class=\"source-row\">\
                 <input name=\"sources\" type=\"url\" value=\"{}\" placeholder=\"https://source\" />\
                 <button type=\"button\" class=\"remove-source\">Remove</button>\
                 </div>",
                escape_html(source)
            )
        })
        .collect::<String>();
    let source_rows = if source_rows.is_empty() {
        "<div class=\"source-row\">\
         <input name=\"sources\" type=\"url\" placeholder=\"https://source\" />\
         <button type=\"button\" class=\"remove-source\" hidden>Remove</button>\
         </div>"
            .to_owned()
    } else {
        source_rows
    };

    let radio = |value: &str, label: &str| {
        let checked = if significance == value {
            " checked"
        } else {
            ""
        };
        format!(
            "<label><input type=\"radio\" name=\"significance\" value=\"{value}\"{checked} /> {label}</label>"
        )
    };

    let form_action = format!("/api/timeline/{}/suggestions", escape_html(timeline_id));
    let timeline_href = format!("/timeline/{}", escape_html(timeline_id));

    format!(
        r#"<main>
  <p class="eyebrow">Suggest a missing event</p>
  <h1>Suggest a missing event</h1>
  <p class="lede">Propose a dated fact the Timeline is missing. The Timeline's owner reviews it before it appears.</p>
  {summary}
  <form method="post" action="{form_action}">
    <input type="hidden" name="kind" value="APPEND" />

    {date_field}

    {title_field}

    {description_field}

    <fieldset>
      <legend>Sources</legend>
      <p class="help">At least one http(s) URL is required. A claim without a source cannot be reviewed.</p>
      <div id="source-rows" data-source-rows>{source_rows}</div>
      <button type="button" class="add-source" id="add-source">Add another source</button>
    </fieldset>

    <fieldset>
      <legend>Significance</legend>
      <p class="help">Regular is the default. Minor is background detail, Crucial is a turning point.</p>
      <div class="significance">
        {radio_minor}
        {radio_regular}
        {radio_crucial}
      </div>
    </fieldset>

    <div class="form-actions">
      <button type="submit">Submit suggestion</button>
      <a class="form-cancel" href="{timeline_href}">Cancel and go back to the Timeline</a>
    </div>
  </form>
</main>
{script}"#,
        summary = summary,
        form_action = form_action,
        date_field = field_input(
            "date",
            "Date",
            &format!(
                "<input id=\"date\" name=\"date\" type=\"date\" value=\"{}\"{} />",
                escape_html(date),
                invalid_attribute(errors, "date")
            ),
            "The exact date of the event you are proposing.",
            errors,
            "date",
        ),
        title_field = field_input(
            "title",
            "Title",
            &format!(
                "<input id=\"title\" name=\"title\" value=\"{}\"{} />",
                escape_html(title),
                invalid_attribute(errors, "title")
            ),
            "A short headline for the event.",
            errors,
            "title",
        ),
        description_field = field_input(
            "description",
            "Description",
            &format!(
                "<textarea id=\"description\" name=\"description\"{}>{}</textarea>",
                invalid_attribute(errors, "description"),
                escape_html(description)
            ),
            "What happened, and why it matters to the story.",
            errors,
            "description",
        ),
        radio_minor = radio("minor", "Minor"),
        radio_regular = radio("regular", "Regular"),
        radio_crucial = radio("crucial", "Crucial"),
        source_rows = source_rows,
        timeline_href = timeline_href,
        script = SOURCE_ROWS_SCRIPT,
    )
}

/// The "suggest a missing event" form as a complete document.
pub(super) fn append_form_html(
    timeline_id: &str,
    submitted: &PayloadRequest,
    errors: &[FieldError],
) -> String {
    let body = append_form_body(timeline_id, submitted, errors);
    PageDocument::new("Suggest a missing event", &body)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .script("/assets/app.js")
        .render()
}

/// Confirmation for a proposed new Event: it is pending, and nothing has
/// changed on the Timeline yet.
pub(super) fn suggestion_appended_html(timeline_id: &str, submitted: &PayloadRequest) -> String {
    let timeline = escape_html(timeline_id);
    let title = submitted.title.as_deref().unwrap_or("your event");
    let body = format!(
        "<main>\
         <p class=\"eyebrow\">Suggestion submitted</p>\
         <h1>Thank you &mdash; your suggestion is pending review</h1>\
         <p>The Timeline's owner will check <strong>{title}</strong> and its sources before it is added. \
         Nothing appears on the Timeline until then.</p>\
         <p><a href=\"/timeline/{timeline}\">Back to the Timeline</a></p>\
         </main>",
        title = escape_html(title),
    );
    PageDocument::new("Suggestion submitted", &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
}

/// Plain-language label for a stored significance value.
fn significance_label(value: &str) -> &'static str {
    match value.trim() {
        "minor" => "Minor",
        "crucial" => "Crucial",
        _ => "Regular",
    }
}

/// The suggest form as a complete document for the route's Timeline and Event.
pub(super) fn suggest_form_html(
    timeline_id: &str,
    event: &Event,
    submitted: &PayloadRequest,
    errors: &[FieldError],
) -> String {
    let body = suggest_form_body(timeline_id, event, submitted, errors);
    PageDocument::new("Suggest an edit", &body)
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .script("/assets/app.js")
        .render()
}

/// Read-your-writes confirmation: the suggestion is stored and pending.
pub(super) fn suggestion_submitted_html(timeline_id: &str, event: &Event) -> String {
    let timeline = escape_html(timeline_id);
    let body = format!(
        "<main>\
         <p class=\"eyebrow\">Suggestion submitted</p>\
         <h1>Thank you &mdash; your suggestion is pending review</h1>\
         <p>The Timeline's owner will compare your correction to the Event \
         <strong>{title}</strong> ({date}) before it is applied. Nothing on the \
         Timeline changes until then.</p>\
         <p><a href=\"/timeline/{timeline}#event-{event_id}\">Back to the Event</a> \
         &middot; <a href=\"/timeline/{timeline}\">Back to the Timeline</a></p>\
         </main>",
        title = escape_html(&event.title),
        date = escape_html(&event.date),
        event_id = escape_html(&event.event_id),
    );
    PageDocument::new("Suggestion submitted", &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
}

/// A refusal that cannot be fixed on the form (missing Event, refused write).
pub(super) fn suggestion_notice_html(timeline_id: &str, heading: &str, message: &str) -> String {
    let timeline = escape_html(timeline_id);
    let body = format!(
        "<main>\
         <h1>{}</h1>\
         <p>{}</p>\
         <p><a href=\"/timeline/{timeline}\">Back to the Timeline</a></p>\
         </main>",
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
    use data::model::{EditPayload, SuggestionKind, SuggestionState};

    use super::super::payload::prefilled;

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

    fn sample_event() -> Event {
        Event {
            timeline_id: "t1".to_owned(),
            event_id: "e1".to_owned(),
            timestamp: Timestamp::from_date("2026-02-04").expect("date"),
            date: "2026-02-04".to_owned(),
            title: "Candidate filing deadline".to_owned(),
            description: "Petitions were due.".to_owned(),
            sources: vec!["https://example.org/one".to_owned()],
            significance: data::model::Significance::Crucial,
            upvotes: 0,
            downvotes: 0,
            report_count: 0,
            flagged: false,
            created_at: Timestamp::parse("2026-11-08T12:34:56.789Z").expect("test timestamp"),
            created_by: "owner".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        }
    }

    #[test]
    fn the_form_page_renders_through_the_shared_shell() {
        let event = sample_event();
        let html = suggest_form_html("t1", &event, &prefilled(&event), &[]);
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
    fn the_form_is_prefilled_from_the_current_event() {
        let event = sample_event();
        let html = suggest_form_html("t1", &event, &prefilled(&event), &[]);
        assert!(
            html.contains("value=\"2026-02-04\""),
            "the date starts at the current date"
        );
        assert!(
            html.contains("type=\"date\""),
            "a date input, not free text"
        );
        assert!(html.contains("value=\"Candidate filing deadline\""));
        assert!(html.contains(">Petitions were due.</textarea>"));
        assert!(
            html.contains("value=\"https://example.org/one\""),
            "the current sources start in the source rows"
        );
        assert!(
            html.contains("value=\"crucial\" checked"),
            "the current significance is the checked radio"
        );
    }

    #[test]
    fn every_field_says_what_unchanged_means() {
        let event = sample_event();
        let html = suggest_form_html("t1", &event, &prefilled(&event), &[]);
        assert_eq!(
            html.matches("leave it to keep it unchanged").count()
                + html
                    .matches("Leave it as it is to keep it unchanged")
                    .count(),
            4,
            "date, title, description and significance each state the rule"
        );
        assert!(html.contains("Currently <strong>Crucial</strong>"));
        assert!(html.contains("Leave them as they are to keep them unchanged"));
    }

    #[test]
    fn a_refused_submission_keeps_what_was_typed_and_points_at_the_field() {
        let event = sample_event();
        let submitted = PayloadRequest {
            date: Some("February 4".to_owned()),
            title: Some("Candidate filing deadline".to_owned()),
            ..prefilled(&event)
        };
        let errors = [FieldError {
            field: "payload.date".to_owned(),
            message: "exact date as YYYY-MM-DD is required".to_owned(),
        }];
        let html = suggest_form_html("t1", &event, &submitted, &errors);
        assert!(html.contains("role=\"alert\""), "the summary is announced");
        assert!(
            html.contains("exact date as YYYY-MM-DD is required"),
            "the message is on the page"
        );
        assert!(
            html.contains("aria-invalid=\"true\" aria-describedby=\"date-error\""),
            "the failing field points at its message"
        );
        assert!(
            html.contains("id=\"date-error\""),
            "the message is attached to the field"
        );
    }

    #[test]
    fn the_confirmation_page_says_pending_review_and_links_back() {
        let event = sample_event();
        let html = suggestion_submitted_html("t1", &event);
        assert!(html.contains("pending review"));
        assert!(html.contains("href=\"/timeline/t1#event-e1\""));
        assert!(html.contains("href=\"/timeline/t1\""));
        assert_eq!(html.matches("<style").count(), 1, "one stylesheet");
        assert!(html.contains("noindex"));
    }

    #[test]
    fn the_form_page_escapes_route_ids_and_event_text() {
        let mut event = sample_event();
        event.title = "<script>alert('x')</script>".to_owned();
        let html = suggest_form_html("t\"><script>", &event, &prefilled(&event), &[]);
        assert!(!html.contains("<script>alert"));
        assert!(
            !html.contains("t\"><script>"),
            "escaped ids cannot break out of an attribute"
        );
        assert!(html.contains("&quot;"), "the quote is escaped");
        assert!(html.contains("&lt;script&gt;"), "event text is escaped");
    }
    #[test]
    fn the_append_form_starts_empty_and_states_what_is_required() {
        let html = append_form_html("t1", &PayloadRequest::default(), &[]);
        assert!(html.contains("value=\"APPEND\""));
        assert!(html.contains("action=\"/api/timeline/t1/suggestions\""));
        assert!(html.contains("Suggest a missing event"));
        assert!(
            html.contains("type=\"date\""),
            "a date input, not free text"
        );
        assert!(
            html.contains("At least one http(s) URL is required"),
            "the source rule is stated before submission"
        );
        assert!(
            html.contains("Regular is the default"),
            "the significance default is stated"
        );
        assert!(html.contains("value=\"regular\" checked"));
        assert!(html.contains("Cancel and go back to the Timeline"));
        assert!(
            !html.contains("targetEventId"),
            "an append targets no event"
        );
    }

    #[test]
    fn a_refused_append_keeps_the_typed_values_and_points_at_the_field() {
        let submitted = PayloadRequest {
            date: Some("March 1".to_owned()),
            title: Some("A missing fact".to_owned()),
            ..PayloadRequest::default()
        };
        let errors = [FieldError {
            field: "payload.date".to_owned(),
            message: "exact date as YYYY-MM-DD is required".to_owned(),
        }];
        let html = append_form_html("t1", &submitted, &errors);
        assert!(html.contains("role=\"alert\""));
        assert!(html.contains("Your suggestion was not sent"));
        assert!(
            html.contains("value=\"A missing fact\""),
            "the title survives"
        );
        assert!(html.contains("value=\"March 1\""), "the bad date survives");
        assert!(html.contains("aria-invalid=\"true\" aria-describedby=\"date-error\""));
    }
}
