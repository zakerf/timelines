//! The one document every owner refusal uses.
//!
//! The queue, the reports summary, Timeline settings, cover failures and the
//! Collection forms all refuse with a page rather than a JSON body. They differ
//! only in the link they offer back, so the chrome lives here.

use renderer::shell::{NOTICE_CSS, PageDocument};

use crate::http::escape_html;

/// The way back offered by a refusal page.
pub(crate) struct BackLink<'a> {
    pub href: &'a str,
    pub label: &'a str,
}

/// A light, `noindex` notice with a heading, a message and one back link,
/// inside the shared document shell.
pub(crate) fn page(heading: &str, message: &str, back: BackLink<'_>) -> String {
    let body = format!(
        "<main><h1>{}</h1><p>{}</p><p><a href=\"{}\">{}</a></p></main>",
        escape_html(heading),
        escape_html(message),
        escape_html(back.href),
        escape_html(back.label),
    );
    PageDocument::new(heading, &body)
        .stylesheet(NOTICE_CSS)
        .noindex()
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_carries_the_heading_message_and_back_link() {
        let html = page(
            "Not yours to change",
            "Only the owner can do that.",
            BackLink {
                href: "/collection/c1",
                label: "Back to the collection",
            },
        );
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<h1>Not yours to change</h1>"));
        assert!(html.contains("<p>Only the owner can do that.</p>"));
        assert!(html.contains("<a href=\"/collection/c1\">Back to the collection</a>"));
        assert!(html.contains("noindex"), "a refusal is never indexed");
    }

    #[test]
    fn the_page_escapes_every_interpolated_value() {
        let html = page(
            "<script>",
            "\"quoted\"",
            BackLink {
                href: "/x\"><script>",
                label: "<b>",
            },
        );
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("\"quoted\""));
        assert!(!html.contains("<b>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
