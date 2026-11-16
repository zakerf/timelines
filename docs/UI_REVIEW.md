# UI/UX review — timelines

Date: 2026-11-15
Scope: every surface a person can reach today (rendered pages + API-served forms), measured against the
product surface in `plans/timeline-site/PRD.md` §3.

Method: read the renderer (`crates/renderer/src/ui.rs`, `page.rs`, `ssg.rs`), the API routes and
response builders (`crates/api/src/**`), the local proxy routing (`nginx/default.conf`), and the
actual generated documents in `target/site/` (home, collection, timeline, 404). HTMX behaviour was
checked against the vendored 2.0.4 bundle. I did not run the Docker stack; the API-side findings are
from the response-building code paths, which are unambiguous.

Severity:

- **P0** — a normal user action ends somewhere broken or does nothing, with no recovery.
- **P1** — a surface the PRD promises is absent, or a reader/author cannot complete an ordinary task.
- **P2** — present but unintuitive, inconsistent, or inaccessible.
- **P3** — polish.

---

## 1. What the UI covers today

### Pages a visitor can land on

| Route | Rendered by | What is on it |
|---|---|---|
| `/` | `ui.rs:73` | "Timelines" H1, root collection links, up to 12 recent timelines |
| `/collection/:id` | `ui.rs:142` | collection title/deck, child collections, timelines grouped by update year, "Back to all Timelines" |
| `/timeline/:id` | `ui.rs:294` | hero + spine of event cards + add/suggest affordances |
| `/404.html` | `ssg.rs:33` | dark-themed "Page not found", one link home |
| `/api/timeline` (GET) | `publish/mod.rs:780` | publish form (login required) |
| `/api/timeline/:id/events/new` (GET) | `publish/mod.rs:828` | add-event form (owner only) |
| `/api/timeline/:id/event/:eid/suggest` (GET) | `suggestions/view.rs:133` | edit-suggestion form (login required) |

That is the entire user-facing surface. Everything else is JSON or a bare fragment.

### API surface with no UI

| Capability | Endpoint | UI |
|---|---|---|
| Sign in / sign out / who am I | `/api/auth/login`, `/logout` (POST only), `/me` | **none** — no link, no account state |
| Import a timeline document | `POST /api/timeline/import` | none |
| Cover upload / replace / remove | `POST|DELETE /api/timeline/:id/cover` | none |
| Collection create / rename / delete / membership | `POST /collections`, `PATCH|DELETE /collections/:id`, `PUT /timeline/:id/collection` | none (publish form asks for a raw id) |
| Pending suggestions (public) | `GET /api/timeline/:id/suggestions` | none |
| Owner moderation queue | `GET /api/suggestions/queue` | none |
| Approve / reject a suggestion | `POST .../approve`, `.../reject` | none |
| Append a suggestion | `POST /api/timeline/:id/suggestions` with `kind=APPEND` | none (only EDIT has a form) |
| Report / withdraw / owner report summary | `POST|DELETE .../report`, `GET .../reports` | none |
| Owner's own vote state | `GET .../votes` | none (static page never paints it) |

The pattern: **the write model is complete, the human interfaces for it are not.** Of the nine
in-scope product surfaces in PRD §3 (timelines, events, sources, collections, suggestions, votes,
accounts, covers, reports), only reading and the two narrow forms have a human interface; accounts,
collections, covers, reports, and all moderation are API only.

---

## 2. Broken journeys (fix before anything else)

These are not missing features. They are the features you have, ending in a place a person cannot
use.

### 2.1 Publishing from the browser ends on raw JSON or an unstyled fragment — P0

The publish page is a plain `<form method="post" action="/api/timeline">` with no HTMX
(`publish/mod.rs:805`). The browser therefore navigates to whatever the API returns:

- **Success** → `created_fragment()` (`publish/mod.rs:291`), a `<article>` with no `<html>`, no
  stylesheet, no navigation, served as a `201`. The journalist lands on an unstyled fragment and must
  click a bare "Open the timeline" link.
- **Any validation failure** → `publish_timeline` has no browser branch (`publish/mod.rs:162-172`);
  `PublishError::Checklist` becomes `field_errors_response()`, i.e. a JSON envelope
  (`publish/mod.rs:767`). **The entire draft is lost**, and the person sees
  `{"error":"checklist_failed","errors":[...]}` in the browser.

This is the primary authoring flow of a news site. It is currently unusable in a browser.
The append flow at least has `append_form_error()` (`publish/mod.rs:618`) — publish does not.

Fix: detect the form content type the way `append_event` does, re-render the form with the submitted
values and per-field messages, `303` to the new timeline on success. Keep the JSON contract for
`Accept: application/json`.

### 2.2 Suggesting an edit ends on a JSON document — P0

`SUGGEST_FORM_HTML` is a plain form posting to `/api/timeline/:id/suggestions`
(`suggestions/view.rs:166`). Success returns `json_response(201, suggestion_json)`
(`suggestions/mod.rs:203`); every refusal returns JSON (`suggestions/error.rs:30-60`). So a reader
who writes a careful correction gets a JSON blob, with no confirmation, no "pending review" state,
and no way back to the timeline.

Compounding it, the form (`suggestions/view.rs:170-199`):

- **does not show the current values** — to fix a typo in a title you must retype or copy the whole
  title from another tab;
- has no diff/preview, so the submitter cannot check what they are claiming;
- uses free-text `YYYY-MM-DD` with no date input;
- explains "leave blank to keep unchanged", which makes clearing a field impossible by design;
- loses everything on a validation error if the error path is ever made browser-friendly.

Fix: prefill all fields from the event, diff against the original on submit, render a "Suggestion
submitted — pending review" page with a link back, and render errors inline on the form.

### 2.3 Voting silently does nothing for most people — P0

`hx-post` to the vote endpoint requires `Identity` (`vote/mod.rs:54-96`). The responses are
`plain()` refusals: `401 "sign in to continue"`, `403 "you cannot vote on your own timeline"`,
`429` (`vote/mod.rs:338-374`). HTMX 2.0.4's default `responseHandling` is
`[{204 no swap}, {2xx swap}, {4xx/5xx no swap, error:true}]` and the page registers no error
handler, so:

- anonymous reader clicks ▲ → **nothing happens at all** (no prompt to sign in);
- the timeline owner clicks ▲ → **nothing happens** (self-vote is 403);
- a rate-limited reader → **nothing happens**.

And even on success the user cannot tell their vote registered:

- the static widget has no `data-my-vote` and the stylesheet has no `[data-my-vote]` rule
  (`ui.rs:481-537`, `STYLES`), so the swapped fragment from the API carries state that nothing
  renders;
- nothing calls `GET /api/timeline/:id/votes`, so a returning voter sees no selected state on load;
- up and down are two unrelated counters; there is no net score, no percentage, no total;
- the "×" clear button is always visible and unlabelled visually, sitting between the down count and
  the widget label;
- buttons are `font-size: 0.85rem` with `padding: 0` — roughly a 12–14 px hit target.

Fix: add a body-level `htmx:responseError` handler that turns 401 into a sign-in link and 403/429
into a visible message; paint `data-my-vote`; fetch own votes on load for signed-in users; rebuild
the control as up / score / down with the clear action revealed only when a vote exists; give
buttons ≥24 px targets and `:focus-visible`.

### 2.4 "Add an event here" is a lie, and it is owner-only — P0

`TimelineDocument` inserts a pill after **every** event except the last (`ui.rs:298-319`) plus one at
the end (`ui.rs:337`), all pointing at `/api/timeline/:id/events/new`.

Two problems:

1. **The position is meaningless.** Events are stored under `EVENT#<timestamp>#<id>` and queried in
   date order (`data/src/keys.rs:50`, `renderer/src/dynamo.rs`). The append endpoint takes no
   position (`publish/mod.rs:370`). An event added "here" between two dates reappears wherever its
   date sorts. The affordance promises insertion the model does not support.
2. **Every reader sees the owner's tools.** On a three-event demo page there are three
   "+ Add an event" pills. An anonymous reader who clicks one is bounced through Keycloak and then
   told "Only the timeline owner can add events." (`publish/mod.rs:626`).

Fix: either rename to "Add an event" and place it once (end of timeline), or implement insertion
(needs a position/fractional index). Render the owner controls only after a client-side
`/api/auth/me` check, and give readers the public action they actually need: **suggest a new
event** (see 3.5).

### 2.5 The publish form accepts a raw collection id that silently creates phantom collections — P0

`Collection id (optional)` is a free-text input (`publish/mod.rs:812`). There is no collection
picker, no listing, no create flow. If the id is wrong, `Site::collection_ids()` includes any
referenced id (`page.rs:35-49`) and the renderer emits `/collection/<typo>` with the **id as its
title** (`ui.rs:152-155`). A typo produces a live, linked collection page named
`electoins-2026` with an empty description. Nothing surfaces the mistake.

Fix: `<select>`/datalist populated from existing collections, plus "New collection…" inline.
Never render a collection whose row does not exist.

### 2.6 There is no sign-in or sign-out anywhere — P0/P1

No page links to `/api/auth/login`. The only way in is to trigger a protected action. `/api/auth/logout`
is POST-only (`auth/mod.rs:35`), so it cannot be a link and there is no form for it. After signing in
you land on `/` with no indication you are signed in, no name, no way out, and no owner tools. The
account row, display name, and session all exist; none of it reaches the page.

Fix: a persistent header with `Sign in`, or the display name + `Sign out` (a one-button POST form),
rendered client-side from `/api/auth/me` since the pages are static.

### 2.7 A site with no timelines cannot be rendered — P1

`renderer/src/main.rs:91-98` errors out when the table has no timelines. So the first deploy of an
empty environment fails, and Home has no empty state. Home should render "no timelines yet" with a
call to publish.

---

## 3. Missing completely

Ordered by how much it blocks the product.

### 3.1 Global chrome and identity — P1

There is no site header, no navigation, no footer, no logo/site name, no skip link, and no account
area on any page. Each document is an island:

- Home has no link to Publish or Sign in.
- Collection has one "Back to all Timelines".
- Timeline has no link back to Home or to its collection.

This matters more than usual because search, tags, and "browse affordances beyond collections" are
deliberately fog (PRD §13). With those deferred, persistent chrome and cross-links are the entire
discovery layer — and they are absent.

### 3.2 Timeline-page chrome — P1

Missing from the read page: the collection name as a **link** (it is plain text in the eyebrow,
`ui.rs:374-376`), last-updated date, author identity, event count, a breadcrumb, a "more in this
collection" block at the end, a permalink/anchor per event, and a print/share affordance.
The page is a dead end: after the last card the only onward links are external sources.

### 3.3 Browsing data that makes lists useful — P1

`TimelineEntry` (`ui.rs:244-258`) shows title, deck, and `"{n} events"`. There is no cover
thumbnail (despite covers being the flagship visual), no date span ("Feb–Nov 2026"), no
last-updated date, no author, no rating. Home caps at 12 with no "see all" and no pagination.
Collection pages do not paginate either.

### 3.4 Authoring beyond a pipe-delimited textarea — P1

The only way to create a timeline is:

```
YYYY-MM-DD | Title | https://source[,https://source2] | Description
```

one event per line, in a monospace textarea (`publish/mod.rs:816`). Missing: a per-event composer,
add/remove/reorder rows, per-event significance, a preview, a draft, per-field errors that preserve
input, source-count validation feedback, a collection picker, and cover upload. The import endpoint
exists but has no UI and is not linked.

There is also **no way to edit a published timeline**: no rename, no deck change, no collection move,
no cover (API only), no unpublish/delete. Yet the 404 copy says "may be unpublished or deleted"
(`ssg.rs:60`), and `Collection`/membership mutation endpoints exist with no UI.

### 3.5 Suggesting — P1

- **Append suggestions do not exist in the UI.** The API supports `kind=APPEND`, but the only form
  is EDIT (`suggestions/view.rs:167`). The between-event affordances go to the owner's direct-append
  form instead. PRD §3 says suggestion entry points appear "both per Event and between Events"; the
  between-Events entry point is the wrong one.
- **The public pending list is invisible.** PRD §3/§4 promise the pending queue is publicly visible.
  `GET /api/timeline/:id/suggestions` returns JSON and no page calls it.
- No confirmation/status after submitting, no "my suggestions", no indication that a timeline has
  pending corrections.

### 3.6 Owner moderation — P1

`/api/suggestions/queue` is JSON. There is no page to read a pending suggestion, compare it to the
current event, write a rejection note, and approve/reject. The owner's core ongoing job is API-only.
No pending-count badge anywhere.

### 3.7 Reports and flags — P1

Report endpoints exist for timeline, event, and suggestion, plus withdraw and an owner summary. No
page has a Report control, no flagged badge, no owner report view. The PRD's "owner sees a flag once
3 accounts report it" has no surface.

### 3.8 Collections management — P1

Create/rename/nest/delete/membership are API-only. Combined with 2.5, collections can only be
created by curl, and Home/Collection lists are read-only.

### 3.9 Feedback, loading, and status states — P1

No loading indicator, no disabled submit, no inline success, no toast/status region, no optimistic
update, no `hx-indicator`. Because HTMX does not swap 4xx, the only visible feedback on a vote is
"nothing". On slow local/edge conditions every action looks dead.

### 3.10 Empty, error, and not-found states — P1/P2

- 404 is the only designed error page, and it is **dark** while the whole product is warm paper
  (`ssg.rs:41-55`) — it reads like a different site.
- The router's fallback renders `<p class="missing">Page not found.</p>` with no document shell or
  styles (`ui.rs:53`).
- Collection empty state is one italic sentence, no action.
- No empty/error state for Home (build fails, 2.7).
- Rate limits and validation on vote/append/suggest surface as raw text/JSON.

### 3.11 Social, SEO, and site identity — P2

No `og:`/`twitter:` tags, no canonical URL, no favicon, no `robots.txt`, no `sitemap.xml`, no
structured data. Sharing a timeline produces a bare link; the baked vote counts are there for SEO but
nothing else a crawler/social card looks for. `<title>` is `"{title} - timeline"` /
`"{title} - collection"` (lowercase, no site name) and `"Timelines"` on Home — inconsistent.

### 3.12 Account surface — P2

`/api/auth/me` is JSON. There is no profile page, no "my timelines", no "my suggestions", no
"signed in as". (Author public pages are fog; an owner dashboard is not.)

---

## 4. Existing surfaces that need improvement

### 4.1 Hero

- The eyebrow is either the collection title (not a link) or the literal word **"Timeline"**
  (`ui.rs:374-376`). A generic label where a navigational parent should be.
- Byline is `owner_name.unwrap_or(owner_id)` (`ui.rs:377`): if the account row is missing, readers
  see `By demo-author` or a raw opaque subject id. Needs a human fallback and no leaked ids.
- No "Updated <date>", no event count, no reading time.
- On mobile the hero is at least `30rem` tall before any content.
- The rendered HTML contains Leptos `<!>` placeholders (`hero__inner`, inside `sources`, around the
  byline) that survive `strip_hydration_bookkeeping` (`ssg.rs:334`). Harmless but sloppy in source
  and reproducible in every document.

### 4.2 Event cards

- **Dates are rendered as raw ISO** (`ui.rs:449-451`): `2026-02-04`, and for date-time events the
  full `2026-02-04T09:00:00Z` string. For a reading product this should be "4 February 2026" at
  minimum, with the ISO value kept in `datetime`.
- **The crucial ring is unexplained.** `spine__card--crucial` is an accent border plus shadow
  (`ui.rs:815-822`) with no legend, label, or tooltip. A reader has no way to learn what the ring
  means, and nothing in the publish UI can set it (the pipe parser hardcodes `significance: None`,
  `publish/mod.rs:156`), so it is unreachable for new content.
- **Source chips show only the host** (`page.rs:175-186`). Two sources on the same domain are
  indistinguishable; there is no source title, no numbering, no "primary" marker, no total count.
  Non-http sources render as an identical-looking chip with `opacity: 0.8` and no explanation.
- Footer puts the vote widget and "Suggest an edit" at opposite ends with no hierarchy; the edit link
  is 0.8rem underlined muted text.
- No event permalink/anchor, so a specific fact cannot be cited.

### 4.3 Vote widget

Beyond 2.3:

- Reading order is `▲ 58 ▼ 10 × Timeline rating` — two counters, a destructive-looking clear, then a
  jargon label. The label "Timeline rating" / "Event accuracy" sounds like a grade, but the control
  is a two-way vote with no displayed score.
- `aria-label` is on a plain `div` with no `role="group"` (`ui.rs:494-503`).
- No selected/pressed state, no hover affordance beyond opacity, no focus styles.
- The first-paint markup and the API fragment differ (`data-my-vote` only in the API version), so the
  two must be kept in sync by hand — and the CSS half was never written.

### 4.4 The spine and the add pills

- One add pill per gap (2.4) makes the owner's tools the loudest repeated element on a reader page.
- `.spine__add` is an `<li>` inside the ordered list of events (`ui.rs:304`), so the list's sequence
  contains non-events; screen readers announce controls as list items.
- **The spine motif collapses at narrow widths.** The node is `left: -2.41rem` from a card that sits
  at the reading column's edge (`ui.rs:804-814`); measured in a 484 px viewport the node lands at
  `x = -28.2px`, entirely off-canvas. Cards are opaque, so the connecting line is also hidden in the
  gaps. What remains is a plain stack of cards — the one layout idea that distinguishes the product
  is invisible on a phone.
- Drop the dashed pill styling on a reading surface or move the action to the end.

### 4.5 Home

- Section is titled "Recently updated" but shows **no updated date** (`ui.rs:130`, `ui.rs:255`), so
  the ordering is invisible and unverifiable.
- `recent.truncate(12)` with no "see all" (`ui.rs:84`).
- Collections are listed before timelines even when empty; if there are no root collections the
  section disappears with no explanation.
- No covers, authors, ratings, or date spans — the home page looks like an index of stubs.
- Counts read "1 events" (no pluralization).

### 4.6 Collection page

- Timelines are grouped under year headings derived from **last-updated** year
  (`page.rs:86-128`). A reader will read "2026 / 2025" as when the story happened; it is when the
  page was edited. Misleading grouping; the year of the events themselves would be meaningful, or no
  grouping at all.
- No breadcrumb, no parent-collection link, no timeline count, no owner actions.
- A nested collection page shows child collections and direct timelines, but nothing indicates the
  hierarchy on the child page.
- Missing collection pages are rendered as a real page titled with the id (2.5).

### 4.7 Forms

Shared problems across publish/append/suggest:

- No site shell: no header, no back/cancel, no link to the timeline you are editing.
- Error copy on append says "Fix the fields below and try again." (`publish/mod.rs:635`) but the
  fields are on a previous page and the input is gone.
- Errors are not announced (`aria-live`, focus management, `aria-invalid`) and not inline.
- Free-text dates (`<input placeholder="YYYY-MM-DD">`) instead of `type="date"`.
- No submit pending state; a double submit is possible.
- Labels are inconsistent: "Deck" on publish (`publish/mod.rs:809`), "Description" in the model,
  "lede" in the Home markup. Vocabulary should be the domain's ("description").
- Source rows are added by an inline script that duplicates the same 30 lines in two templates
  (`publish/mod.rs:895-929`, `suggestions/view.rs:204-238`); no validation hints beyond `type=url`.
- The suggest form's significance radios are unexplained ("Minor / Regular / Crucial") and include a
  fourth "Leave unchanged" that only exists because of the blank-means-unchanged rule.

### 4.8 Visual system and consistency

- The palette and type tokens are copied into six separate inline stylesheets
  (`ui.rs:542`, `ui.rs:645`, plus four form/error templates in `publish/mod.rs` and
  `suggestions/view.rs`). They are already drifting: the 404 is dark; form pages use a different
  heading scale and no focus treatment.
- No `:focus-visible` rules anywhere; no `prefers-color-scheme`; no `prefers-reduced-motion` (no
  motion exists, so fine for now).
- Every page inlines the full stylesheet and re-downloads the 51 KB HTMX bundle — acceptable, but it
  also means no shared cache and no single place to fix the design.

### 4.9 Copy and jargon

| Where | Says | Problem |
|---|---|---|
| Publish | "Deck" | model calls it description; PRD says description |
| Publish | "One Event per line: `YYYY-MM-DD \| Title \| https://source...`" | developer syntax as the primary instruction |
| Publish | "Collection id (optional)" | internal key exposed as user input |
| Suggest | "The owner reviews your suggestion before it is applied." | true, but no status after submit |
| Suggest | "Leave a field blank to keep it unchanged." | cannot clear a field |
| Event footer | "Suggest an edit" | no "suggest a missing event" counterpart |
| Vote | "Timeline rating" / "Event accuracy" | sounds like a score, is a two-way vote |
| Event | crucial ring | never explained |
| 404 | "This Timeline may be unpublished or deleted" | wrong for any non-timeline 404 |

### 4.10 Mobile

- Vote controls are far below the 24 px minimum target.
- Hero `min-height: 30rem` plus a full-bleed scrim before any content is heavy on a phone.
- Repeated add pills between cards add visual noise on narrow screens.
- No sticky context (collection name, timeline title) while scrolling a long timeline.

---

## 5. Accessibility findings

- **Keyboard focus is invisible.** No `:focus-visible`/`:focus` styles; the vote buttons are
  unstyled buttons with no outline override, so UA outline may appear on some browsers but the
  design provides none.
- **Hit targets**: vote arrows ≈ 0.85rem with zero padding; clear "×" ≈ 0.72rem. Fails WCAG 2.5.8.
- **List semantics**: add pills are `<li>` children of the event `<ol>`.
- **Widget semantics**: `aria-label` on a `div` with no role; the group is not announced as a group,
  and state (`data-my-vote`) is not exposed via `aria-pressed`.
- **Link names**: multiple identical "Add an event here" links with no positional context.
- **No landmarks/skip link**: pages have one `<main>` but no `<nav>`, no skip-to-content, and no
  consistent header.
- **Error handling**: no `role="alert"`/`aria-live`; HTMX swaps nothing on error and announces
  nothing.
- **New-tab links**: source chips use `target="_blank"` with no indication in the label.
- **Color**: crucial is signalled by accent border + ring (not color alone, good) but is unexplained.
  Vote directions use green/red plus glyphs (okay).
- **Headings**: order is sane on all three static pages (h1 → h2 → h3).
- **Meta description** exists on collection and timeline; Home has none.

---

## 6. Contradictions between what the UI promises and what the system does

1. "Add an event **here**" vs date-ordered storage (2.4).
2. "Recently updated" with no dates (4.5).
3. Collection "years" that are edit years (4.6).
4. Public pending suggestions promised by the PRD, invisible in the product (3.5).
5. Reports promised to flag content, no report control (3.7).
6. "Only the timeline owner can add events" as the destination of a public CTA (2.4).
7. 404 copy referencing unpublished/deleted timelines that cannot be unpublished/deleted (3.4).
8. Cover images as a product feature, absent from every browsing surface (3.3).

---

## 7. Deliberately out of scope (do not treat as defects)

Per PRD §13: full-text search, tags, browse affordances beyond collections, RSS/embeds/public API,
branding/domain, i18n, author handles/uniqueness, wiki-style edit history and per-event attribution,
community review of suggestions, covers on collections, voting on collections, fuzzy dates.
The gaps in §3.1–3.12 are not these; they are surfaces the MVP already has endpoints for, or chrome
that any site needs.

---

## 8. Recommended sequence

### Phase 0 — stop the bleeding (small, high value)

1. **Publish form**: browser-aware success (`303` to `/timeline/:id`) and failure (form + preserved
   values + inline errors). *Acceptance: submit with an invalid date; you stay on a styled form with
   your text and a message next to the date.*
2. **Suggest form**: prefill from the event, success page ("pending review"), inline errors.
   *Acceptance: no JSON ever appears as a document from a form.*
3. **Vote feedback**: global HTMX error handler → 401 becomes a "Sign in to vote" link, 403/429 a
   message; add `data-my-vote` styling and load own votes on page load.
4. **Add-event affordances**: one "Add an event" at the end; remove per-position pills until
   insertion exists; hide owner controls for non-owners.
5. **Collection id**: replace free text with a picker; never render a collection page for an id with
   no row.
6. **Empty site**: render Home with an empty state instead of failing the build.

### Phase 1 — the missing surfaces

7. **Global header** on every static page and form: site name → `/`, and a client-painted auth area
   (`/api/auth/me`): Sign in, or name + Sign out (POST form). Add "New timeline".
8. **Timeline page chrome**: link the collection eyebrow; show updated date, author, event count;
   add "More in this collection" and a link home at the end; add OG/Twitter/canonical.
9. **Public append suggestion**: a "Suggest a missing event" form and entry point at the end of the
   timeline; render the pending suggestions list on the page (fetched, not baked); show submission
   status.
10. **Owner surfaces**: a queue page at `/suggestions/queue` (HTML) with approve/reject + note; a
    pending badge on the timeline; cover upload/remove; timeline settings (title, description,
    collection, unpublish); "My timelines".
11. **Collections UI**: create/rename/nest/delete and add/remove timelines; show them in the header
    or home.
12. **Reports**: a Report control on timeline/event/suggestion with confirm + withdraw; owner flag
    summary.

### Phase 2 — make it feel finished

13. Human-readable dates everywhere (keep ISO in `datetime`); pluralize counts; show event date
    spans on list entries.
14. Redesign the vote control (up / score / down, active state, clear only when voted, ≥24 px
    targets, `aria-pressed`, focus styles).
15. Source chips: label with a title when available, number them, disclose new-tab.
16. Significance: explain it, make it settable at publish, or drop the ring until it means something.
17. Fix list semantics, add skip link and landmarks, add visible focus, announce errors.
18. One shared stylesheet/token source; bring the 404 into the light theme.
19. Home/Collection: covers as thumbnails, pagination or "see all", meaningful grouping.
20. Social/SEO: favicon, OG image (hero cover), `robots.txt`, `sitemap.xml`, JSON-LD.

---

## 9. Quick wins (each under a day)

- Link the hero eyebrow to `/collection/:id`.
- Render dates as "4 February 2026".
- Pluralize "1 event" / "3 events".
- Add `data-my-vote` CSS + `aria-pressed` and load own votes.
- Add a header "Sign in" / "Sign out" (POST form) driven by `/api/auth/me`.
- Give vote buttons padding and `:focus-visible`.
- Add `title` to source chips and number them.
- Retheme 404 light; link Home from it.
- Add `og:title`, `og:description`, `og:image`, canonical.
- Show "Updated <date>" under each Home entry.
- Replace the free-text collection id with a datalist.
- Add a "Cancel" link on every form.

---

## 10. One-paragraph summary

The read page's craft is fine — the Spine layout, hero, and typography are the strongest part of the
product. What is missing surrounds it: no persistent chrome, no account state, no authoring or
moderation surface, no reporting, no collections management, no pending-suggestion visibility, and no
public append suggestion. What is present is often wired to an API shape rather than a human one:
publishing and suggesting dump JSON or a bare fragment into the browser, voting fails silently for
everyone who is not a signed-in non-owner, and "add an event here" promises an insertion the data
model cannot do. Fix the four form/vote journeys first, then build the header and the owner
surfaces; the visual layer can stay as it is.

---

## Appendix: browser-verified measurements

Served `target/site` with the same extensionless rewrite as `nginx/default.conf` and inspected the
three static pages in Chromium (viewport 484×885, i.e. a phone-sized window):

| Check | Measured |
|---|---|
| Timeline page title / H1 | `Ohio Senate 2026 - timeline`, one H1 |
| Header links (`<header> a`) | **0** |
| `<nav>` elements | **0** (all three pages) |
| Skip link | **0** |
| Favicon / OG / canonical | **0** (all three pages) |
| Add-event pills on 3 events | **3** ("here" ×2 + "at the end") |
| Vote button size | up/down **11.7 × 13.6 px**, clear **7.9 × 11.5 px** |
| `:focus-visible` rules in the inline stylesheet | **0** |
| `[data-my-vote]` rules | **0** |
| Event date text | `2026-02-04` (ISO, `datetime` identical) |
| Hero height | **566 px** of an 885 px viewport (64%) before any event |
| Spine node (`::before`) | `left: -28.2px` against a card at `x=18` → **clipped off-canvas**; the vertical spine line is hidden behind the opaque cards between gaps |
| Home page body | H1 "Timelines", "Collections", one collection, one timeline row |
| Collection page headings | H1 "Elections 2026" → H2 "Timelines" → H3 "2026" (the edit year, not the story year) |

The spine measurement is the important new one: the signature "vertical spine with nodes" does not
render at narrow widths — the nodes fall outside the viewport and the connecting line is covered by
the cards. The layout reads as a plain stack of cards with dashed pills between them.

