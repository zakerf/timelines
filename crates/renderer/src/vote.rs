//! The one vote widget, shared by the static renderer and the API fragment.
//!
//! A Timeline page bakes its counts into the static document, and a vote swaps
//! the widget for the API's fresh fragment. Both come from [`widget_html`], so
//! the first paint and a client-side refresh cannot drift apart: there is one
//! builder and two callers (`crate::ui::VoteCounts` and
//! `crates/api/src/vote/mod.rs`).
//!
//! The shape follows the UI review (docs/UI_REVIEW.md §2.3, §4.3, §5):
//!
//! - one net score instead of two unexplained counters (the raw `data-up` /
//!   `data-down` values stay for the API contract and the HTMX refresh);
//! - a clear action that only appears once the reader has voted;
//! - `aria-pressed` on both direction buttons and `data-my-vote` on the widget,
//!   so the chosen direction is visible and survives a reload;
//! - a labelled group and a polite status element that
//!   [`crate::ssg::APP_ASSET_PATH`] fills with the reason a refusal happened;
//! - one neutral prompt ("Is this event accurate?") rather than a jargon
//!   rating, so the control does not read like a grade.

use data::model::VoteDirection;

use crate::shell::escape;

/// `data-my-vote` when the reader has no vote on the target.
pub const MY_VOTE_NONE: &str = "none";
/// `data-my-vote` after an upvote.
pub const MY_VOTE_UP: &str = "up";
/// `data-my-vote` after a downvote.
pub const MY_VOTE_DOWN: &str = "down";

/// The Timeline-level prompt. Neutral by design: a two-way vote, not a rating.
pub const TIMELINE_LABEL: &str = "Is this timeline accurate?";
/// The Event-level prompt. Same wording shape as [`TIMELINE_LABEL`].
pub const EVENT_LABEL: &str = "Is this event accurate?";

/// One widget's inputs. Everything a caller knows: where to post, which target
/// the own-vote read paints, the baked counters, the prompt, and the reader's
/// own vote when the caller knows it (the static page does not).
#[derive(Clone, Copy, Debug)]
pub struct VoteWidget<'a> {
    /// HTMX endpoint, e.g. `/api/timeline/{id}/vote`.
    pub endpoint: &'a str,
    /// The id `GET /api/timeline/{id}/votes` keys on: a Timeline id for the
    /// Timeline widget, an Event id for an Event widget.
    pub target: &'a str,
    /// Baked upvotes.
    pub upvotes: i64,
    /// Baked downvotes.
    pub downvotes: i64,
    /// Reader-facing prompt, one of [`TIMELINE_LABEL`] / [`EVENT_LABEL`].
    pub label: &'a str,
    /// The reader's own vote, when the caller knows it.
    pub my_vote: Option<VoteDirection>,
}

impl VoteWidget<'_> {
    /// The net score the widget displays: upvotes minus downvotes.
    pub fn score(&self) -> i64 {
        self.upvotes - self.downvotes
    }

    /// The `data-my-vote` value: `up`, `down`, or `none`.
    pub fn my_vote_value(&self) -> &'static str {
        match self.my_vote {
            Some(VoteDirection::Up) => MY_VOTE_UP,
            Some(VoteDirection::Down) => MY_VOTE_DOWN,
            None => MY_VOTE_NONE,
        }
    }
}

/// The complete widget, ready to be swapped in by HTMX or baked into a page.
///
/// Every interpolated value is escaped here, because both callers render the
/// result as raw HTML (`inner_html` on the Leptos side, a `text/html` body on
/// the API side).
pub fn widget_html(widget: &VoteWidget<'_>) -> String {
    let endpoint = escape(widget.endpoint);
    let target = escape(widget.target);
    let label = escape(widget.label);
    let my_vote = widget.my_vote_value();
    let up = widget.upvotes;
    let down = widget.downvotes;
    let score = widget.score();
    let score_text = score_text(score);
    let up_pressed = pressed(widget.my_vote == Some(VoteDirection::Up));
    let down_pressed = pressed(widget.my_vote == Some(VoteDirection::Down));
    // The clear action is present but hidden until the reader has a vote, so
    // the markup, the CSS, and `app.js` all agree without rebuilding the node.
    let clear_hidden = if widget.my_vote.is_some() {
        ""
    } else {
        " hidden"
    };

    format!(
        concat!(
            r#"<div class="vote" data-vote-target="{target}" data-up="{up}" data-down="{down}" "#,
            r#"data-score="{score}" data-my-vote="{my_vote}" role="group" aria-label="{label}" "#,
            r#"hx-target="this" hx-swap="outerHTML">"#,
            r#"<button type="button" class="vote__btn vote__btn--up" data-vote-direction="up" "#,
            r#"aria-label="Upvote" aria-pressed="{up_pressed}" hx-post="{endpoint}" "#,
            r#"hx-vals='{{"direction":"up"}}'>▲</button>"#,
            r#"<span class="vote__score">"#,
            r#"<span class="vote__score-number" aria-hidden="true">{score_text}</span>"#,
            r#"<span class="vote__sr">Score {score} from {up} up and {down} down</span>"#,
            r#"</span>"#,
            r#"<button type="button" class="vote__btn vote__btn--down" data-vote-direction="down" "#,
            r#"aria-label="Downvote" aria-pressed="{down_pressed}" hx-post="{endpoint}" "#,
            r#"hx-vals='{{"direction":"down"}}'>▼</button>"#,
            r#"<button type="button" class="vote__btn vote__btn--clear" "#,
            r#"aria-label="Withdraw your vote" hx-post="{endpoint}" "#,
            r#"hx-vals='{{"direction":"none"}}'{clear_hidden}>×</button>"#,
            r#"<span class="vote__label">{label}</span>"#,
            r#"<p class="vote__status" role="status" aria-live="polite" hidden></p>"#,
            r#"</div>"#
        ),
        target = target,
        up = up,
        down = down,
        score = score,
        my_vote = my_vote,
        label = label,
        endpoint = endpoint,
        score_text = score_text,
        up_pressed = up_pressed,
        down_pressed = down_pressed,
        clear_hidden = clear_hidden,
    )
}

fn pressed(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// `+48` for a positive net score, `-5` below zero, `0` when tied.
fn score_text(score: i64) -> String {
    if score > 0 {
        format!("+{score}")
    } else {
        score.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::READ_CSS;

    fn widget(my_vote: Option<VoteDirection>) -> VoteWidget<'static> {
        VoteWidget {
            endpoint: "/api/timeline/ohio-senate-2026/vote",
            target: "ohio-senate-2026",
            upvotes: 58,
            downvotes: 10,
            label: TIMELINE_LABEL,
            my_vote,
        }
    }

    #[test]
    fn a_neutral_widget_marks_nothing_pressed_and_hides_the_clear_action() {
        let html = widget_html(&widget(None));

        assert!(html.contains("data-my-vote=\"none\""), "{html}");
        assert!(
            html.contains("aria-pressed=\"false\" hx-post=\"/api/timeline/ohio-senate-2026/vote\" hx-vals='{\"direction\":\"up\"}'"),
            "the up button is not pressed: {html}"
        );
        assert!(
            html.contains("aria-pressed=\"false\" hx-post=\"/api/timeline/ohio-senate-2026/vote\" hx-vals='{\"direction\":\"down\"}'"),
            "the down button is not pressed: {html}"
        );
        assert!(
            html.contains("hx-vals='{\"direction\":\"none\"}' hidden>×</button>"),
            "the clear action exists but is hidden without a vote: {html}"
        );
    }

    #[test]
    fn a_cast_vote_marks_the_direction_and_reveals_the_clear_action() {
        let up = widget_html(&widget(Some(VoteDirection::Up)));
        assert!(up.contains("data-my-vote=\"up\""), "{up}");
        assert!(
            up.contains("data-vote-direction=\"up\" aria-label=\"Upvote\" aria-pressed=\"true\""),
            "the up button is pressed: {up}"
        );
        assert!(
            up.contains(
                "data-vote-direction=\"down\" aria-label=\"Downvote\" aria-pressed=\"false\""
            ),
            "the down button is not pressed: {up}"
        );
        assert!(
            up.contains("hx-vals='{\"direction\":\"none\"}'>×</button>"),
            "the clear action is revealed: {up}"
        );

        let down = widget_html(&widget(Some(VoteDirection::Down)));
        assert!(down.contains("data-my-vote=\"down\""), "{down}");
        assert!(
            down.contains(
                "data-vote-direction=\"down\" aria-label=\"Downvote\" aria-pressed=\"true\""
            ),
            "the down button is pressed: {down}"
        );
        assert!(
            down.contains(
                "data-vote-direction=\"up\" aria-label=\"Upvote\" aria-pressed=\"false\""
            ),
            "{down}"
        );
    }

    #[test]
    fn a_widget_is_a_labelled_group_with_a_net_score_and_the_baked_counters() {
        let html = widget_html(&widget(None));

        assert!(
            html.contains("role=\"group\" aria-label=\"Is this timeline accurate?\""),
            "the widget announces itself as a named group: {html}"
        );
        assert!(
            html.contains("data-up=\"58\"") && html.contains("data-down=\"10\""),
            "the API's counters stay on the widget: {html}"
        );
        assert!(html.contains("data-score=\"48\""), "net score attribute");
        assert!(
            html.contains(">+48</span>"),
            "the score is the net total, with its sign: {html}"
        );
        assert!(
            html.contains("Score 48 from 58 up and 10 down"),
            "the score has a spoken form: {html}"
        );
        assert!(
            !html.contains("vote__count"),
            "the two unexplained counters are gone: {html}"
        );
    }

    #[test]
    fn a_negative_net_score_keeps_its_sign() {
        let widget = VoteWidget {
            upvotes: 3,
            downvotes: 8,
            ..widget(None)
        };
        let html = widget_html(&widget);

        assert_eq!(widget.score(), -5);
        assert!(html.contains("data-score=\"-5\""), "{html}");
        assert!(html.contains(">-5</span>"), "{html}");
        assert!(html.contains("Score -5 from 3 up and 8 down"), "{html}");
    }

    #[test]
    fn every_button_meets_the_touch_target_minimum_and_has_a_visible_focus_rule() {
        let html = widget_html(&widget(None));
        assert_eq!(
            html.matches("class=\"vote__btn vote__btn--").count(),
            3,
            "up, down and clear all carry the shared button class: {html}"
        );

        assert!(
            READ_CSS.contains("min-width: 24px") && READ_CSS.contains("min-height: 24px"),
            "the vote buttons are at least 24x24 CSS px (WCAG 2.5.8)"
        );
        assert!(
            READ_CSS.contains(".vote__btn:focus-visible"),
            "the vote buttons keep a visible focus ring on the dark hero too"
        );
        assert!(
            READ_CSS.contains(".vote[data-my-vote=\"up\"] .vote__btn--up"),
            "the chosen direction is painted from data-my-vote"
        );
    }

    #[test]
    fn a_refusal_status_element_is_polite_and_starts_hidden() {
        let html = widget_html(&widget(None));
        assert!(
            html.contains("class=\"vote__status\" role=\"status\" aria-live=\"polite\" hidden"),
            "{html}"
        );
    }

    #[test]
    fn the_widget_escapes_every_interpolated_value() {
        let html = widget_html(&VoteWidget {
            endpoint: "/api/timeline/x\"><script>alert(1)</script>/vote",
            target: "x\"><script>alert(2)</script>",
            upvotes: 1,
            downvotes: 0,
            label: "<script>alert(3)</script>",
            my_vote: None,
        });

        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"), "{html}");
        assert_eq!(
            html.matches("&lt;script&gt;").count(),
            6,
            "endpoint (posted by three buttons), target and label are all escaped: {html}"
        );
    }

    #[test]
    fn timeline_and_event_widgets_differ_only_in_endpoint_target_and_prompt() {
        let event = widget_html(&VoteWidget {
            endpoint: "/api/timeline/ohio-senate-2026/event/primary-day/vote",
            target: "primary-day",
            upvotes: 58,
            downvotes: 10,
            label: EVENT_LABEL,
            my_vote: None,
        });

        assert!(
            event.contains("data-vote-target=\"primary-day\""),
            "{event}"
        );
        assert!(
            event.contains("class=\"vote\" data-vote-target=\"primary-day\""),
            "the same widget shape as the Timeline control: {event}"
        );
        assert!(event.contains("Is this event accurate?"), "{event}");
        assert_eq!(
            event.matches("class=\"vote__btn vote__btn--").count(),
            3,
            "{event}"
        );
        assert!(
            !TIMELINE_LABEL.contains("rating") && !EVENT_LABEL.contains("accuracy,\""),
            "the prompts do not read like a grade"
        );
    }
}
