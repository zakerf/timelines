//! The one document shell: tokens, base CSS, and the plain-string page builder.
//!
//! These tests pin the shell's contract for every surface that is not rendered
//! through Leptos (the API's forms and refusals) plus the CSS facts that make
//! "change a token once" true.

use renderer::shell::{
    BASE_CSS, FORM_CSS, INDEX_CSS, NOTICE_CSS, PageDocument, READ_CSS, TOKENS_CSS, stylesheet,
};

/// Every stylesheet block the shell knows about, by name, so a new block cannot
/// quietly copy a token.
fn blocks() -> [(&'static str, &'static str); 5] {
    [
        ("base", BASE_CSS),
        ("index", INDEX_CSS),
        ("read", READ_CSS),
        ("notice", NOTICE_CSS),
        ("form", FORM_CSS),
    ]
}

#[test]
fn the_palette_and_type_stack_are_defined_once() {
    for token in [
        "--paper: #faf8f4",
        "--ink: #17171a",
        "--muted: #5c5a60",
        "--line: #ddd6c9",
        "--accent: #a63a23",
        "--serif: Georgia",
        "--sans: \"Segoe UI\"",
    ] {
        assert_eq!(
            TOKENS_CSS.matches(token).count(),
            1,
            "the token block defines {token:?} exactly once"
        );
    }

    for (name, css) in blocks() {
        assert!(
            !css.contains("--paper:"),
            "{name} must not redefine the palette"
        );
        assert!(
            !css.contains("--serif:") && !css.contains("--sans:"),
            "{name} must not redefine the type stack"
        );
        assert!(
            !css.contains("Georgia,") && !css.contains("\"Segoe UI\""),
            "{name} must use var(--serif)/var(--sans), not a second stack"
        );
        assert!(
            !css.contains(":root"),
            "{name} must not open a second token block"
        );
    }
}

#[test]
fn the_base_block_owns_the_document_and_focus_styles() {
    assert!(BASE_CSS.contains("* { box-sizing: border-box; }"));
    assert!(BASE_CSS.contains("background: var(--paper);"));
    assert!(BASE_CSS.contains("font-family: var(--sans);"));
    assert!(
        BASE_CSS.contains(":focus-visible"),
        "focus styles exist in exactly one place"
    );
}

#[test]
fn a_page_has_exactly_one_stylesheet_source() {
    let html = PageDocument::new("Title", "<main><p>Body</p></main>")
        .stylesheet(NOTICE_CSS)
        .stylesheet(FORM_CSS)
        .render();

    assert!(html.starts_with("<!DOCTYPE html>"), "complete document");
    assert!(html.contains("<html lang=\"en\">"));
    assert!(html.contains("<title>Title</title>"));
    assert_eq!(html.matches("<style").count(), 1, "one style element");
    assert_eq!(
        html.matches("--paper: #faf8f4").count(),
        1,
        "one token block per document"
    );
    assert!(
        html.contains("<main><p>Body</p></main>"),
        "body is verbatim"
    );

    let tokens = html.find("--paper: #faf8f4").expect("tokens are emitted");
    let base = html.find("* { box-sizing: border-box; }").expect("base");
    let notice = html.find("main {").expect("notice block");
    assert!(
        tokens < base && base < notice,
        "the shared tokens and base come before page blocks"
    );
}

#[test]
fn documents_are_noindex_script_free_or_carry_scripts_on_request() {
    let quiet = PageDocument::new("Gone", "<main></main>")
        .noindex()
        .stylesheet(NOTICE_CSS)
        .render();
    assert!(quiet.contains("<meta name=\"robots\" content=\"noindex\" />"));
    assert!(!quiet.contains("<script"));

    let read = PageDocument::new("Read", "<main></main>")
        .script("/assets/htmx.min.js")
        .render();
    assert!(read.contains("<script src=\"/assets/htmx.min.js\" defer></script>"));
    assert!(!read.contains("noindex"));
}

#[test]
fn titles_and_descriptions_are_escaped() {
    let html = PageDocument::new("<X>", "<main></main>")
        .description("\"quoted\" & <tag>")
        .render();
    assert!(html.contains("<title>&lt;X&gt;</title>"));
    assert!(
        html.contains("content=\"&quot;quoted&quot; &amp; &lt;tag&gt;\""),
        "attribute text is escaped: {html}"
    );
}

#[test]
fn a_stylesheet_is_the_tokens_base_and_the_page_blocks_in_order() {
    let css = stylesheet(&[NOTICE_CSS, FORM_CSS]);
    assert!(css.starts_with(TOKENS_CSS));
    assert!(css.contains(BASE_CSS));
    assert_eq!(css.matches("--paper: #faf8f4").count(), 1);
    let notice = css.find("main {").expect("notice");
    let form = css.find("button[type=\"submit\"]").expect("form");
    assert!(notice < form, "page blocks keep their order");

    assert_eq!(stylesheet(&[]), format!("{TOKENS_CSS}{BASE_CSS}"));
}
