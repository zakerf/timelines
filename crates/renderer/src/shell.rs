//! The one document shell and design-token source for every HTML surface.
//!
//! Two flavours share this module so a token, a base style, or a piece of
//! chrome changes once and reaches every page:
//!
//! - [`DocumentShell`], a Leptos component for the statically generated pages
//!   ([`crate::ui`]);
//! - [`PageDocument`], a plain-string builder for the API's `format!` pages
//!   (the publish / append / suggest forms and every HTML refusal).
//!
//! Both emit the same document: `<!DOCTYPE html>` → `<html lang="en">` →
//! `<head>` (charset, viewport, optional robots/description, title, one
//! `<style>`, the site script, optional page scripts) → `<body>` → the one
//! site header → page body.
//!
//! Every page's stylesheet is [`stylesheet`] applied to page blocks: the
//! palette/type tokens ([`TOKENS_CSS`]) and the document base ([`BASE_CSS`])
//! always come first, so no page block may define `:root`, a `--token`, or a
//! second serif/sans stack.
//!
//! ## The one site header
//!
//! [`HEADER_INNER_HTML`] is rendered inside `<header class="site-header">` by
//! both flavours, so every static page and every API-rendered form/refusal
//! carries the same chrome and no page emits its own. The header is static
//! HTML: the site name always links Home and the account area's first paint is
//! the anonymous "Sign in" link, so a page is never wrong (and never invents an
//! identity) when [`APP_SCRIPT_PATH`] does not run. That script upgrades the
//! account area from `GET /api/auth/me` and reveals `data-owner-only` controls
//! only after the viewer passes the per-Timeline ownership check.
//!
//! Owner-only controls ship as `hidden` + `data-owner-only` +
//! `data-timeline-id`, and [`BASE_CSS`] makes `[hidden]` win over any page
//! layout rule so they can never appear before the check.

use leptos::prelude::*;

/// Palette, type scale, and colour scheme. The only place these are defined.
pub const TOKENS_CSS: &str = r#"
:root {
  color-scheme: light;
  --paper: #faf8f4;
  --ink: #17171a;
  --muted: #5c5a60;
  --line: #ddd6c9;
  --accent: #a63a23;
  --serif: Georgia, "Iowan Old Style", "Times New Roman", serif;
  --sans: "Segoe UI", system-ui, -apple-system, sans-serif;
}
"#;

/// Document base shared by every page: border-box, the paper ground, the sans
/// body face, the one focus treatment, the one `hidden` rule, and the one site
/// header (chrome every page carries, see [`HEADER_INNER_HTML`]).
pub const BASE_CSS: &str = r#"
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body {
  margin: 0;
  background: var(--paper);
  color: var(--ink);
  font-family: var(--sans);
  line-height: 1.6;
  -webkit-font-smoothing: antialiased;
}
:where(a, button, input, textarea, select):focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}
/* Owner-only controls ship `hidden` and are revealed by the site script, so
   `hidden` has to beat any layout rule a page sets on the same element. */
[hidden] { display: none !important; }
.site-header {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  justify-content: space-between;
  gap: 0.6rem 1.5rem;
  padding: 0.85rem 1.5rem;
  border-bottom: 1px solid var(--line);
}
.site-header__brand {
  font-family: var(--serif);
  font-size: 1.1rem;
  font-weight: 700;
  letter-spacing: -0.01em;
  color: var(--ink);
  text-decoration: none;
}
.site-header__brand:hover { color: var(--accent); }
.site-header__account {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.35rem 1rem;
  margin: 0;
  font-size: 0.88rem;
}
.site-header__account a { color: var(--muted); }
.site-header__account a:hover { color: var(--accent); }
.site-header__account form { margin: 0; }
.site-header__account button {
  padding: 0;
  border: 0;
  background: none;
  color: var(--muted);
  font: inherit;
  text-decoration: underline;
  cursor: pointer;
}
.site-header__account button:hover { color: var(--accent); }
.site-header__name { color: var(--ink); font-weight: 600; }
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip-path: inset(50%);
  white-space: nowrap;
  border: 0;
}
"#;

/// Served path of the site's one page script: account chrome plus every
/// client-side upgrade (votes, owner controls). The renderer emits the bundle
/// beside the pages (see `ssg::generate_site`), so it works the same for a
/// static deploy and an API-rendered form page.
pub const APP_SCRIPT_PATH: &str = "/assets/app.js";

/// The inside of `<header class="site-header">`, shared verbatim by both
/// flavours so the chrome cannot drift.
///
/// - the site name links Home;
/// - the account area is the anonymous first paint: a `Sign in` link with `next`
///   defaulting to Home (the site script rewrites it to the current path);
/// - the signed-in pieces exist but are `hidden` and empty, so a page visited
///   without the site script never claims an identity;
/// - `Sign out` is a POST form because the endpoint refuses `GET`.
pub const HEADER_INNER_HTML: &str = r#"<a class="site-header__brand" href="/">Timelines</a>
<nav class="site-header__account" aria-label="Account" data-account>
  <a class="site-header__signin" href="/api/auth/login?next=/" data-sign-in>Sign in</a>
  <span class="site-header__name" data-account-name hidden></span>
  <a class="site-header__link" href="/api/timeline" data-new-timeline hidden>New timeline</a>
  <form class="site-header__signout" method="post" action="/api/auth/logout" data-sign-out hidden><button type="submit">Sign out</button></form>
</nav>"#;

/// Home and Collection: the index surface (hero band, section headings, entry
/// rows). Class names are frozen by the generated pages.
pub const INDEX_CSS: &str = r#"
.index__hero {
  padding: 4rem 1.5rem 2.25rem;
  border-bottom: 1px solid var(--line);
}
.index__eyebrow {
  margin: 0 0 0.7rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--accent);
}
.index__heading {
  margin: 0;
  font-family: var(--serif);
  font-size: clamp(2.5rem, 6vw, 4rem);
  line-height: 1.05;
  letter-spacing: -0.02em;
}
.index__lede {
  margin: 0.9rem 0 0;
  max-width: 46rem;
  font-family: var(--serif);
  font-size: clamp(1.05rem, 2vw, 1.3rem);
  line-height: 1.5;
  color: #3a383f;
}
.index {
  max-width: 48rem;
  margin: 0 auto;
  padding: 1.5rem 1.35rem 5rem;
}
.index__section { margin-top: 2.25rem; }
.index__section:first-child { margin-top: 1.25rem; }
.index__section-title {
  margin: 0 0 0.4rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--accent);
}
.index__group { margin-top: 1.35rem; }
.index__group-title {
  margin: 0 0 0.2rem;
  font-family: var(--serif);
  font-size: 1.3rem;
  color: var(--muted);
}
.index__empty {
  margin: 0.75rem 0 0;
  color: var(--muted);
  font-style: italic;
}
.index__cta { margin: 0.6rem 0 0; }
.index__cta a {
  color: var(--accent);
  font-weight: 600;
  text-decoration: none;
  border-bottom: 1px solid currentColor;
}
.index__cta a:hover { color: var(--ink); }
.index__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.index__item {
  padding: 1.35rem 0;
  border-bottom: 1px solid var(--line);
}
.index__item--collection { padding-top: 1rem; padding-bottom: 1rem; }
.index__row {
  display: flex;
  align-items: flex-start;
  gap: 1rem;
}
.index__thumb {
  flex: 0 0 auto;
  width: 7rem;
  height: 4.5rem;
  object-fit: cover;
  border-radius: 2px;
  background-color: #121216;
}
.index__thumb--gradient {
  display: block;
  background-image: linear-gradient(135deg, var(--gradient-from), var(--gradient-to));
}
.index__body {
  flex: 1 1 auto;
  min-width: 0;
}
.index__title {
  font-family: var(--serif);
  font-size: 1.5rem;
  color: var(--ink);
  text-decoration: none;
}
.index__title:hover { color: var(--accent); }
.index__deck { margin: 0.4rem 0 0; color: #2a292e; }
.index__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0 0.7rem;
  margin: 0.45rem 0 0;
  font-size: 0.78rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}
.index__meta > * + * {
  padding-left: 0.7rem;
  border-left: 1px solid var(--line);
}
.index__more {
  margin: 1rem 0 0;
  font-size: 0.95rem;
}
.index__more a {
  color: var(--accent);
}
.index__back { margin-top: 2rem; }
.index__back a { color: var(--muted); }
"#;

/// The Timeline read page: hero, vote widget, spine, Event cards, source
/// chips, and the add/edit affordances. Inline so a page is one uploadable
/// file; no web fonts, no images besides the owner's cover, no transitions, no
/// overlays on the reading column.
pub const READ_CSS: &str = r#"
.hero {
  position: relative;
  display: flex;
  align-items: flex-end;
  min-height: clamp(30rem, 64vh, 44rem);
  overflow: hidden;
  background-color: #121216;
  color: #fff;
}
.hero__cover {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.hero--gradient {
  background-image: linear-gradient(135deg, var(--gradient-from), var(--gradient-to));
}
.hero__scrim {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, rgba(8, 8, 12, 0.18) 0%, rgba(8, 8, 12, 0.82) 100%);
}
.hero__inner {
  position: relative;
  z-index: 1;
  width: 100%;
  max-width: 74rem;
  margin: 0 auto;
  padding: 3rem 1.5rem 3.25rem;
}
.hero__eyebrow {
  margin: 0 0 0.9rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: rgba(255, 255, 255, 0.74);
}
.hero__eyebrow a {
  color: inherit;
  text-decoration: none;
  border-bottom: 1px solid rgba(255, 255, 255, 0.35);
}
.hero__eyebrow a:hover {
  color: #fff;
  border-bottom-color: #fff;
}
.hero__title {
  margin: 0 0 0.7rem;
  max-width: 20ch;
  font-family: var(--serif);
  font-size: clamp(2.75rem, 7.4vw, 5rem);
  font-weight: 700;
  line-height: 1.02;
  letter-spacing: -0.025em;
  text-wrap: balance;
}
.hero__deck {
  margin: 0;
  max-width: 46rem;
  font-family: var(--serif);
  font-size: clamp(1.15rem, 2.1vw, 1.45rem);
  line-height: 1.45;
  color: rgba(255, 255, 255, 0.88);
}
.hero__meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 1rem 1.75rem;
  margin-top: 1.4rem;
}
.hero__byline {
  font-size: 0.92rem;
  color: rgba(255, 255, 255, 0.82);
}
.hero__updated,
.hero__count {
  font-size: 0.78rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(255, 255, 255, 0.7);
}
.hero__credit {
  margin: 1rem 0 0;
  font-size: 0.76rem;
  color: rgba(255, 255, 255, 0.6);
}
/* The vote widget (crates/renderer/src/vote.rs builds the markup):
   up / net score / down, a clear action once the reader has voted, a prompt,
   and a polite status element that app.js fills on a refusal. */
.vote-slot { display: contents; }
.vote {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem 0.55rem;
  font-size: 0.82rem;
  color: var(--muted);
}
.hero .vote { color: rgba(255, 255, 255, 0.9); }
.vote__btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 24px;
  min-height: 24px;
  padding: 0 0.3rem;
  border: 1px solid transparent;
  border-radius: 5px;
  background: none;
  color: inherit;
  font: inherit;
  font-size: 0.95rem;
  line-height: 1;
  cursor: pointer;
}
/* The shell's one focus rule is the accent outline; on the dark hero (and on
   the green/red glyphs) currentColor stays visible in both themes. */
.vote__btn:focus-visible {
  outline: 2px solid currentColor;
  outline-offset: 2px;
}
.vote__btn:hover { background: rgba(23, 23, 26, 0.07); }
.hero .vote__btn:hover { background: rgba(255, 255, 255, 0.16); }
.vote__btn[hidden] { display: none; }
.vote__btn--up { color: #2f6b46; }
.vote__btn--down { color: #9a3220; }
.hero .vote__btn--up { color: #9fe0b4; }
.hero .vote__btn--down { color: #ffb9a6; }
.vote__btn--clear { color: var(--muted); font-size: 0.85rem; }
.hero .vote__btn--clear { color: rgba(255, 255, 255, 0.7); }
/* Own-vote state: both the pressed glyph and a tinted, outlined button, so the
   direction is not signalled by colour alone. */
.vote[data-my-vote="up"] .vote__btn--up,
.vote[data-my-vote="down"] .vote__btn--down {
  border-color: currentColor;
  background: rgba(23, 23, 26, 0.08);
}
.hero .vote[data-my-vote="up"] .vote__btn--up,
.hero .vote[data-my-vote="down"] .vote__btn--down {
  background: rgba(255, 255, 255, 0.2);
}
.vote__score {
  display: inline-flex;
  align-items: baseline;
  gap: 0.25rem;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}
.vote__score-number { font-size: 1rem; color: var(--ink); }
.hero .vote__score-number { color: #fff; }
.vote__label {
  font-size: 0.72rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  opacity: 0.75;
}
.vote__status {
  flex-basis: 100%;
  margin: 0.1rem 0 0;
  font-size: 0.8rem;
  letter-spacing: 0;
  text-transform: none;
  color: var(--ink);
}
.hero .vote__status { color: #fff; }
.vote__status a { color: inherit; text-decoration: underline; text-underline-offset: 0.2em; }
.vote__sr {
  position: absolute;
  width: 1px;
  height: 1px;
  margin: -1px;
  padding: 0;
  border: 0;
  overflow: hidden;
  clip-path: inset(50%);
  white-space: nowrap;
}
.reading {
  position: relative;
  z-index: 2;
  width: 100%;
  max-width: 48rem;
  margin: -2rem auto 5rem;
  padding: 0 1.35rem;
}
.spine {
  margin: 0;
  padding: 0 0 0 2rem;
  list-style: none;
  border-left: 2px solid var(--line);
}
.spine__card {
  position: relative;
  margin: 0 0 0 -2rem;
  padding: 1.5rem 1.65rem;
  background: #fff;
  border: 1px solid var(--line);
  border-radius: 6px;
  box-shadow: none;
  scroll-margin-top: 4.5rem;
}
.spine__card::before {
  content: "";
  position: absolute;
  top: 2.05rem;
  left: -2.41rem;
  width: 0.7rem;
  height: 0.7rem;
  border: 2px solid var(--line);
  border-radius: 50%;
  background: var(--paper);
}
.spine__card--crucial {
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(166, 58, 35, 0.18);
}
.spine__card--crucial::before {
  border-color: var(--accent);
  background: var(--accent);
}
.event__date {
  display: block;
  margin-bottom: 0.2rem;
  font-size: 0.74rem;
  font-weight: 700;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--accent);
}
.event__anchor {
  position: absolute;
  top: 1.1rem;
  right: 1.15rem;
  padding: 0 0.15rem;
  color: var(--muted);
  font-size: 0.9rem;
  line-height: 1;
  text-decoration: none;
  opacity: 0.65;
}
.event__anchor:hover { color: var(--accent); opacity: 1; }
.event__title {
  margin: 0 0 0.6rem;
  font-family: var(--serif);
  font-size: clamp(1.35rem, 3.2vw, 1.65rem);
  line-height: 1.2;
  letter-spacing: -0.01em;
}
.event__body {
  margin: 0 0 1rem;
  max-width: 62ch;
  font-size: 1.02rem;
  color: #2a292e;
}
.sources {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem;
  margin: 0 0 1.15rem;
  padding: 0;
  list-style: none;
}
.source-chip {
  display: inline-block;
  padding: 0.22rem 0.6rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: #fbfaf6;
  color: var(--muted);
  font-size: 0.75rem;
  text-decoration: none;
}
a.source-chip:hover { border-color: var(--muted); color: var(--ink); }
.source-chip--text { opacity: 0.8; }
.source-chip__new-tab {
  font-size: 0.95em;
  opacity: 0.8;
}
.event__footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.9rem;
  padding-top: 0.9rem;
  border-top: 1px solid #eee8dc;
}
.suggest {
  font-size: 0.8rem;
  color: var(--muted);
  text-decoration: underline;
  text-underline-offset: 0.2em;
}
.suggest:hover { color: var(--ink); }
.spine__add, .add-pill--end-row { position: relative; list-style: none; }
.spine__add {
  margin: 0 0 0 -2rem;
  padding: 0.9rem 0 0.9rem 2rem;
}
.add-pill {
  display: inline-flex;
  align-items: center;
  padding: 0.38rem 0.8rem;
  border: 1px dashed #b9b0a1;
  border-radius: 999px;
  background: transparent;
  color: var(--muted);
  font-size: 0.78rem;
  text-decoration: none;
}
.add-pill:hover { border-color: var(--accent); color: var(--accent); }
.add-pill--end {
  margin: 1.5rem 0 0 2rem;
}
.page-end {
  margin: 2.75rem 0 0;
  padding: 1.5rem 0 0;
  border-top: 1px solid var(--line);
}
.page-end__title {
  margin: 0 0 0.5rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--accent);
}
.page-end__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.page-end__item {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.4rem 0.75rem;
  padding: 0.55rem 0;
  border-bottom: 1px solid #eee8dc;
}
.page-end__link {
  font-family: var(--serif);
  font-size: 1.15rem;
  color: var(--ink);
  text-decoration: none;
}
.page-end__link:hover { color: var(--accent); }
.page-end__meta {
  font-size: 0.78rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}
.page-end__home { margin: 1.35rem 0 0; }
.page-end__home a { color: var(--muted); }
.pending {
  margin: 2.25rem 0 0;
  padding: 1.35rem 1.5rem;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: #fff;
}
.pending__title {
  margin: 0 0 0.35rem;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--accent);
}
.pending__empty { margin: 0; color: var(--muted); font-size: 0.92rem; }
.pending__list { margin: 0; padding: 0; list-style: none; }
.pending__item {
  padding: 0.75rem 0;
  border-bottom: 1px solid #eee8dc;
  font-size: 0.92rem;
}
.pending__item:last-child { border-bottom: 0; }
.pending__item-title { margin: 0 0 0.2rem; font-family: var(--serif); font-size: 1.05rem; }
.pending__item-meta {
  margin: 0;
  font-size: 0.78rem;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
.pending__queue { margin: 1rem 0 0; }
.pending__queue-link { color: var(--accent); font-size: 0.9rem; }
@media (max-width: 640px) {
  .hero__inner { padding: 2.25rem 1.15rem 2.5rem; }
  .reading { margin-top: -1.25rem; padding: 0 1rem; }
  .spine { padding-left: 1.35rem; }
  .spine__card, .spine__add { margin-left: -1.35rem; }
  .spine__card { padding: 1.2rem 1.15rem; }
  .spine__card::before { left: -1.76rem; }
  .event__anchor { top: 0.85rem; right: 0.85rem; }
  .add-pill--end { margin-left: 1.35rem; }
}
"#;

/// Pages that are neither an index nor a read page: the not-found document,
/// the router fallback, and the readable body the API's forms and refusals sit
/// in (one comfortable column, serif headings, accent links).
pub const NOTICE_CSS: &str = r#"
main {
  max-width: 46rem;
  margin: 0 auto;
  padding: 3rem 1.35rem 5rem;
}
h1 {
  margin: 0 0 0.4rem;
  font-family: var(--serif);
  font-size: clamp(2rem, 5vw, 3rem);
}
p.lede { margin: 0 0 2rem; color: var(--muted); }
.eyebrow {
  margin: 0 0 0.4rem;
  color: var(--accent);
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.14em;
  text-transform: uppercase;
}
a { color: var(--accent); }
ul { padding-left: 1.2rem; }
.missing { padding: 4rem 1.5rem; font-family: var(--serif); }
"#;

/// Base form styles: labels, controls, fieldsets, and the repeatable source
/// rows shared by the publish, append, and suggest pages.
pub const FORM_CSS: &str = r#"
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
.help { margin: 0.35rem 0 0; color: var(--muted); font-size: 0.82rem; }
.field-error { margin: 0.4rem 0 0; color: var(--accent); font-size: 0.86rem; }
.field-error p { margin: 0.2rem 0 0; }
.form-error-summary {
  margin: 0 0 1.5rem;
  padding: 0.9rem 1rem;
  border: 1px solid var(--accent);
  border-left-width: 4px;
  border-radius: 5px;
  background: #fff;
}
.form-error-summary p { margin: 0; }
.form-error-summary ul { margin: 0.5rem 0 0; padding-left: 1.1rem; }
[aria-invalid="true"] { border-color: var(--accent); }
.form-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 1.2rem;
  margin-top: 1.6rem;
}
.form-actions button[type="submit"] { margin-top: 0; }
.form-actions a { color: var(--muted); }
button[type="submit"] { margin-top: 1.6rem; padding: 0.65rem 1.2rem; border: 0; border-radius: 5px; background: var(--accent); color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
"#;

/// The stylesheet for one page: the shared tokens and base, then each page
/// block in order. This is the only composition point; a page never emits its
/// own `:root`.
pub fn stylesheet(blocks: &[&str]) -> String {
    let mut css = String::with_capacity(
        TOKENS_CSS.len() + BASE_CSS.len() + blocks.iter().map(|block| block.len()).sum::<usize>(),
    );
    css.push_str(TOKENS_CSS);
    css.push_str(BASE_CSS);
    for block in blocks {
        css.push_str(block);
    }
    css
}

/// Escape text for HTML content and quoted attributes. The Leptos flavour gets
/// this from Leptos itself; the string builders ([`PageDocument`] and
/// [`crate::vote::widget_html`]) must not trust their callers.
pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The inside of `<head>` for one page, shared by both flavours so they cannot
/// drift: charset, viewport, optional robots/description, title, one `<style>`,
/// the site script, then any page scripts.
fn head_contents(
    title: &str,
    description: Option<&str>,
    noindex: bool,
    styles: &[&str],
    scripts: &[&str],
) -> String {
    let mut head = String::new();
    head.push_str("<meta charset=\"utf-8\" />\n");
    head.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n");
    if noindex {
        head.push_str("<meta name=\"robots\" content=\"noindex\" />\n");
    }
    if let Some(description) = description {
        head.push_str(&format!(
            "<meta name=\"description\" content=\"{}\" />\n",
            escape(description)
        ));
    }
    head.push_str(&format!("<title>{}</title>\n", escape(title)));
    head.push_str("<style>");
    head.push_str(&stylesheet(styles));
    head.push_str("</style>\n");
    // Chrome and every client-side upgrade come from one deferred bundle, so a
    // page never needs to ask for it and no page can forget it.
    head.push_str(&format!(
        "<script src=\"{}\" defer></script>\n",
        escape(APP_SCRIPT_PATH)
    ));
    for src in scripts {
        // The site script is emitted for every page above; a page that lists it
        // anyway (the read page does) must not get a second copy.
        if *src == APP_SCRIPT_PATH {
            continue;
        }
        head.push_str(&format!(
            "<script src=\"{}\" defer></script>\n",
            escape(src)
        ));
    }
    head
}

/// One complete document for pages that are built as strings (the API's
/// `format!` pages). Start with [`PageDocument::new`], add the page blocks the
/// page needs, and call [`PageDocument::render`].
#[derive(Clone, Debug)]
pub struct PageDocument<'a> {
    title: String,
    description: Option<String>,
    noindex: bool,
    styles: Vec<&'static str>,
    scripts: Vec<&'static str>,
    body: &'a str,
}

impl<'a> PageDocument<'a> {
    /// A document titled `title` whose `<body>` is `body` (already-built HTML).
    pub fn new(title: impl Into<String>, body: &'a str) -> Self {
        Self {
            title: title.into(),
            description: None,
            noindex: false,
            styles: Vec::new(),
            scripts: Vec::new(),
            body,
        }
    }

    /// The `<meta name="description">` for this page.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Ask crawlers not to index this document.
    pub fn noindex(mut self) -> Self {
        self.noindex = true;
        self
    }

    /// Append one page stylesheet block after the shared tokens and base.
    /// Page-specific rules belong in a `const` beside the page that needs them.
    pub fn stylesheet(mut self, css: &'static str) -> Self {
        self.styles.push(css);
        self
    }

    /// Add a deferred `<script src>` after the stylesheet. Read pages pass
    /// HTMX; form and refusal pages pass none.
    pub fn script(mut self, src: &'static str) -> Self {
        self.scripts.push(src);
        self
    }

    /// The complete document, ready to hand to `text/html`.
    pub fn render(&self) -> String {
        let head = head_contents(
            &self.title,
            self.description.as_deref(),
            self.noindex,
            &self.styles,
            &self.scripts,
        );
        let mut html = String::with_capacity(head.len() + self.body.len() + 64);
        html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
        html.push_str(&head);
        html.push_str("</head>\n<body>\n");
        html.push_str("<header class=\"site-header\">");
        html.push_str(HEADER_INNER_HTML);
        html.push_str("</header>\n");
        html.push_str(self.body);
        html.push_str("\n</body>\n</html>\n");
        html
    }
}

/// The Leptos document shell. Every static page renders its whole document
/// through this component, so it always gets the tokens, the base stylesheet,
/// and whatever page blocks it asks for.
///
/// ```ignore
/// view! {
///     <DocumentShell title="Timelines" styles=&[INDEX_CSS]>
///         <main class="index">…</main>
///     </DocumentShell>
/// }
/// ```
#[component]
pub fn DocumentShell(
    #[prop(into)] title: String,
    #[prop(into, optional)] description: Option<String>,
    /// Ask crawlers not to index this document.
    #[prop(default = false)]
    noindex: bool,
    /// Page stylesheet blocks, appended after the shared tokens and base.
    #[prop(default = &[])]
    styles: &'static [&'static str],
    /// Deferred `<script src>` tags, in order.
    #[prop(default = &[])]
    scripts: &'static [&'static str],
    children: Children,
) -> impl IntoView {
    let head = head_contents(&title, description.as_deref(), noindex, styles, scripts);

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head inner_html=head></head>
            <body>
                <header class="site-header" inner_html=HEADER_INNER_HTML></header>
                {children()}
            </body>
        </html>
    }
}
