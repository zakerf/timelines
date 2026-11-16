//! The one report control, shared by every surface that shows reportable
//! content (docs/UI_REVIEW.md §3.7).
//!
//! A reader reports a Timeline, an Event, or a pending Suggestion. Reporting is
//! a deliberate two-step action (open, confirm) because it is not undoable by
//! the person being reported, and nothing content-side happens either way: the
//! owner sees a count and a flag, never who reported.
//!
//! Like the vote widget, the markup ships anonymously in the static document
//! and `assets/app.js` upgrades it: the control starts `hidden` so a page whose
//! script never runs does not show a button that cannot work. The server stays
//! the source of truth for the state, and one REPORT row per account per target
//! makes a second report a no-op.

use crate::shell::escape;

/// One report control's inputs.
#[derive(Clone, Copy, Debug)]
pub struct ReportWidget<'a> {
    /// `POST`/`DELETE` endpoint, e.g. `/api/timeline/{id}/report`.
    pub endpoint: &'a str,
    /// A stable, page-unique id for the target, so the script can remember that
    /// this reader already reported it (e.g. `timeline`, `event-<id>`).
    pub target: &'a str,
    /// What the reader is looking at, in the middle of a sentence: `this
    /// timeline`, `this event`, `this suggestion`.
    pub subject: &'a str,
}

/// The complete control. Every interpolated value is escaped here because the
/// renderer embeds the result as raw HTML.
pub fn widget_html(widget: &ReportWidget<'_>) -> String {
    let endpoint = escape(widget.endpoint);
    let target = escape(widget.target);
    let subject = escape(widget.subject);

    format!(
        concat!(
            r#"<div class="report" data-report-endpoint="{endpoint}" data-report-target="{target}">"#,
            r#"<button type="button" class="report__open" aria-expanded="false" hidden>Report</button>"#,
            r#"<div class="report__confirm" hidden>"#,
            r#"<p class="report__question">Report {subject}? The Timeline's owner sees that it was reported, never who reported it. Nothing is hidden automatically.</p>"#,
            r#"<div class="report__actions">"#,
            r#"<button type="button" class="report__yes">Yes, report it</button>"#,
            r#"<button type="button" class="report__cancel">Cancel</button>"#,
            r#"</div></div>"#,
            r#"<div class="report__done" hidden>"#,
            r#"<span class="report__done-text">You reported {subject}. The owner sees a count, not who you are.</span> "#,
            r#"<button type="button" class="report__withdraw">Withdraw your report</button>"#,
            r#"</div>"#,
            r#"<p class="report__status" role="status" aria-live="polite" hidden></p>"#,
            r#"</div>"#
        ),
        endpoint = endpoint,
        target = target,
        subject = subject,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::READ_CSS;

    fn widget() -> ReportWidget<'static> {
        ReportWidget {
            endpoint: "/api/timeline/ohio-senate-2026/report",
            target: "timeline",
            subject: "this timeline",
        }
    }

    #[test]
    fn the_control_starts_hidden_and_needs_two_steps() {
        let html = widget_html(&widget());

        assert!(
            html.contains(r#"class="report__open" aria-expanded="false" hidden"#),
            "the control waits for the script and for a confirmation: {html}"
        );
        assert!(
            html.contains(r#"class="report__confirm" hidden"#),
            "the confirmation panel is not shown until asked for: {html}"
        );
        assert!(html.contains("Yes, report it"), "{html}");
        assert!(html.contains("Cancel"), "{html}");
        assert!(
            !html.contains("Please") && !html.contains("Do you really"),
            "the confirmation asks a plain question"
        );
    }

    #[test]
    fn the_question_promises_anonymity_and_no_automatic_removal() {
        let html = widget_html(&widget());

        assert!(html.contains("Report this timeline?"), "{html}");
        assert!(
            html.contains("never who reported it"),
            "the reader is told the owner cannot see them: {html}"
        );
        assert!(
            html.contains("Nothing is hidden automatically"),
            "reporting does not remove content: {html}"
        );
    }

    #[test]
    fn the_withdraw_action_is_described_in_full() {
        let html = widget_html(&widget());

        assert!(html.contains(r#"class="report__done" hidden"#), "{html}");
        assert!(
            html.contains("You reported this timeline. The owner sees a count, not who you are."),
            "{html}"
        );
        assert!(
            html.contains("Withdraw your report"),
            "a reader can take the report back: {html}"
        );
    }

    #[test]
    fn the_status_element_is_polite_and_starts_hidden() {
        let html = widget_html(&widget());

        assert!(
            html.contains(r#"class="report__status" role="status" aria-live="polite" hidden"#),
            "{html}"
        );
    }

    #[test]
    fn every_surface_differs_only_in_endpoint_target_and_subject() {
        let event = widget_html(&ReportWidget {
            endpoint: "/api/timeline/ohio-senate-2026/event/primary-day/report",
            target: "event-primary-day",
            subject: "this event",
        });
        assert!(
            event.contains(
                r#"data-report-endpoint="/api/timeline/ohio-senate-2026/event/primary-day/report""#
            ),
            "{event}"
        );
        assert!(
            event.contains(r#"data-report-target="event-primary-day""#),
            "{event}"
        );
        assert!(event.contains("Report this event?"), "{event}");

        let suggestion = widget_html(&ReportWidget {
            endpoint: "/api/timeline/ohio-senate-2026/suggestions/s1/report",
            target: "suggestion-s1",
            subject: "this suggestion",
        });
        assert!(
            suggestion.contains("Report this suggestion?"),
            "{suggestion}"
        );
        assert!(
            suggestion.contains(r#"data-report-target="suggestion-s1""#),
            "{suggestion}"
        );
    }

    #[test]
    fn the_widget_escapes_every_interpolated_value() {
        let html = widget_html(&ReportWidget {
            endpoint: "/api/timeline/x\"><script>alert(1)</script>/report",
            target: "x\"><script>alert(2)</script>",
            subject: "<script>alert(3)</script>",
        });

        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"), "{html}");
        assert_eq!(
            html.matches("&lt;script&gt;").count(),
            4,
            "endpoint, target, and both subject sentences are escaped: {html}"
        );
    }

    #[test]
    fn the_widget_is_styled_by_the_shell_with_a_visible_focus_rule() {
        assert!(
            READ_CSS.contains(".report__open:focus-visible"),
            "the report controls keep a visible focus ring"
        );
        assert!(
            READ_CSS.contains(".report__open"),
            "the shell styles the control, so every surface looks the same"
        );
    }
}
