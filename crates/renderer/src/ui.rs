//! The Spine read page as Leptos views.
//!
//! One complete HTML document per Timeline: full-bleed hero (cover or the
//! deterministic gradient), oversized serif title, deck, byline, Timeline-level
//! vote count, then Event cards on a vertical spine with baked Event-level
//! counts and add/edit affordances.
//!
//! The reading column is intentionally plain: one comfortable measure, high
//! contrast, no overlays and no animation. The hero is the only grand surface.

use leptos::prelude::*;
use leptos_router::SsrMode;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_params_map;
use leptos_router::path;
use leptos_router::static_routes::{StaticParamsMap, StaticRoute};

use data::model::{Emphasis, Event};

use crate::page::{
    Site, TimelinePage, gradient_for, group_timelines_by_year, is_http_source, source_label,
};
use crate::ssg::HTMX_ASSET_PATH;

/// The whole app for one render: a router with a single statically-prerendered
/// read route. `Site` is provided as context so the route resolves the page by
/// the `:id` param without touching DynamoDB during render.
pub fn app(site: Site) -> impl IntoView {
    let ids = site.timeline_ids();
    let static_route = StaticRoute::new().prerender_params(move || {
        let ids = ids.clone();
        async move {
            let mut params = StaticParamsMap::new();
            params.insert("id", ids);
            params
        }
    });
    let collection_ids = site.collection_ids();
    let collection_route = StaticRoute::new().prerender_params(move || {
        let ids = collection_ids.clone();
        async move {
            let mut params = StaticParamsMap::new();
            params.insert("id", ids);
            params
        }
    });
    // `/` has no params, but a static route must still declare its (empty)
    // prerender list or the generator skips it.
    let home_route = StaticRoute::new().prerender_params(|| async { StaticParamsMap::new() });

    view! {
        <Router>
            <Routes fallback=|| view! { <p class="missing">"Page not found."</p> }>
                <Route path=path!("/") view=HomeRoute ssr=SsrMode::Static(home_route) />
                <Route
                    path=path!("/timeline/:id")
                    view=TimelineRoute
                    ssr=SsrMode::Static(static_route)
                />
                <Route
                    path=path!("/collection/:id")
                    view=CollectionRoute
                    ssr=SsrMode::Static(collection_route)
                />
            </Routes>
        </Router>
    }
}

/// Home: recently updated Timelines plus entry points into the top-level
/// Collections. Same cover-less shell as a Collection page.
#[component]
fn HomeRoute() -> impl IntoView {
    let site = expect_context::<Site>();

    let mut recent = site.pages.clone();
    recent.sort_by(|left, right| {
        right
            .timeline
            .updated_at
            .cmp(&left.timeline.updated_at)
            .then_with(|| left.timeline.id.cmp(&right.timeline.id))
    });
    recent.truncate(12);
    let recent_entries = recent
        .iter()
        .map(|page| view! { <TimelineEntry page=page.clone() /> })
        .collect_view();

    let collections = site.root_collections();
    let collection_entries = collections
        .iter()
        .map(|collection| {
            let href = format!("/collection/{}", collection.id);
            view! {
                <li class="index__item index__item--collection">
                    <a class="index__title" href=href>{collection.title.clone()}</a>
                    <p class="index__deck">{collection.description.clone()}</p>
                </li>
            }
        })
        .collect_view();
    let collections_section = (!collections.is_empty()).then(|| {
        view! {
            <section class="index__section">
                <h2 class="index__section-title">"Collections"</h2>
                <ul class="index__list">{collection_entries}</ul>
            </section>
        }
    });

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <title>"Timelines"</title>
                <style inner_html=INDEX_STYLES></style>
            </head>
            <body>
                <header class="index__hero">
                    <p class="index__eyebrow">"Home"</p>
                    <h1 class="index__heading">"Timelines"</h1>
                    <p class="index__lede">"Browse collections or the Timelines updated most recently."</p>
                </header>
                <main class="index">
                    {collections_section}
                    <section class="index__section">
                        <h2 class="index__section-title">"Recently updated"</h2>
                        <ol class="index__list">{recent_entries}</ol>
                    </section>
                </main>
            </body>
        </html>
    }
}

/// Collection: its child collections and its directly assigned Timelines,
/// grouped by update year. Both come from the same GSI2 partition.
#[component]
fn CollectionRoute() -> impl IntoView {
    let params = use_params_map();
    let site = expect_context::<Site>();
    let id = params.with(|params| params.get("id").unwrap_or_default());

    let collection = site
        .collections
        .iter()
        .find(|collection| collection.id == id)
        .cloned();
    let title = collection
        .as_ref()
        .map(|collection| collection.title.clone())
        .unwrap_or_else(|| id.clone());
    let description = collection
        .as_ref()
        .map(|collection| collection.description.clone())
        .unwrap_or_default();

    let children = site.child_collections(&id);
    let child_entries = children
        .iter()
        .map(|child| {
            let href = format!("/collection/{}", child.id);
            view! {
                <li class="index__item index__item--collection">
                    <a class="index__title" href=href>{child.title.clone()}</a>
                    <p class="index__deck">{child.description.clone()}</p>
                </li>
            }
        })
        .collect_view();
    let children_section = (!children.is_empty()).then(|| {
        view! {
            <section class="index__section">
                <h2 class="index__section-title">"Collections"</h2>
                <ul class="index__list">{child_entries}</ul>
            </section>
        }
    });

    let pages: Vec<TimelinePage> = site
        .pages
        .iter()
        .filter(|page| page.timeline.collection_id.as_deref() == Some(id.as_str()))
        .cloned()
        .collect();
    let groups = group_timelines_by_year(pages);
    let timeline_sections = groups
        .iter()
        .map(|group| {
            let entries = group
                .pages
                .iter()
                .map(|page| view! { <TimelineEntry page=page.clone() /> })
                .collect_view();
            let label = group.label().to_owned();
            view! {
                <section class="index__group">
                    <h3 class="index__group-title">{label}</h3>
                    <ol class="index__list">{entries}</ol>
                </section>
            }
        })
        .collect_view();
    let timeline_body = if groups.is_empty() {
        view! { <p class="index__empty">"No Timelines in this collection yet."</p> }.into_any()
    } else {
        timeline_sections.into_any()
    };

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <meta name="description" content=description.clone() />
                <title>{format!("{title} - collection")}</title>
                <style inner_html=INDEX_STYLES></style>
            </head>
            <body>
                <header class="index__hero">
                    <p class="index__eyebrow">"Collection"</p>
                    <h1 class="index__heading">{title}</h1>
                    <p class="index__lede">{description}</p>
                </header>
                <main class="index">
                    {children_section}
                    <section class="index__section">
                        <h2 class="index__section-title">"Timelines"</h2>
                        {timeline_body}
                    </section>
                    <p class="index__back"><a href="/">"Back to all Timelines"</a></p>
                </main>
            </body>
        </html>
    }
}

/// One Timeline summary row on Home or a Collection page.
#[component]
fn TimelineEntry(page: TimelinePage) -> impl IntoView {
    let id = page.timeline.id.clone();
    let title = page.timeline.title.clone();
    let description = page.timeline.description.clone();
    let href = format!("/timeline/{id}");
    let event_count = page.events.len();

    view! {
        <li class="index__item">
            <a class="index__title" href=href>{title}</a>
            <p class="index__deck">{description}</p>
            <p class="index__meta">{format!("{event_count} events")}</p>
        </li>
    }
}

/// Resolve the Timeline from the route param. Static generation only emits
/// paths that exist, but a missing page still renders a harmless document.
#[component]
fn TimelineRoute() -> impl IntoView {
    let params = use_params_map();
    let site = expect_context::<Site>();
    let id = params.with(|params| params.get("id").unwrap_or_default());

    match site
        .pages
        .iter()
        .find(|page| page.timeline.id == id)
        .cloned()
    {
        Some(page) => view! { <TimelineDocument page=page /> }.into_any(),
        None => view! {
            <!DOCTYPE html>
            <html lang="en">
                <head>
                    <meta charset="utf-8" />
                    <title>"Timeline not found"</title>
                </head>
                <body>
                    <p class="missing">"Timeline not found."</p>
                </body>
            </html>
        }
        .into_any(),
    }
}

/// A complete, self-contained document: stylesheet inline, HTMX as the only
/// script (no Leptos hydration bundle).
#[component]
fn TimelineDocument(page: TimelinePage) -> impl IntoView {
    let timeline_id = page.timeline.id.clone();
    let events = page.events.clone();
    let event_count = events.len();
    let rows = events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let add_after = (index + 1 < event_count).then(|| {
                view! {
                    <li class="spine__add">
                        <a
                            class="add-pill"
                            href=format!("/api/timeline/{timeline_id}/events/new")
                        >
                            "+ Add an event here"
                        </a>
                    </li>
                }
            });
            view! {
                <EventCard timeline_id=timeline_id.clone() event=event.clone() />
                {add_after}
            }
        })
        .collect_view();
    let end_href = format!("/api/timeline/{timeline_id}/events/new");

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <meta name="description" content=page.timeline.description.clone() />
                <title>{format!("{} - timeline", page.timeline.title)}</title>
                <style inner_html=STYLES></style>
                <script src=HTMX_ASSET_PATH defer></script>
            </head>
            <body>
                <Hero page=page />
                <main class="reading">
                    <ol class="spine">{rows}</ol>
                    <a class="add-pill add-pill--end" href=end_href>
                        "+ Add an event at the end"
                    </a>
                </main>
            </body>
        </html>
    }
}

/// Full-bleed cover hero, or the id-keyed gradient when there is no cover.
#[component]
fn Hero(page: TimelinePage) -> impl IntoView {
    let timeline = page.timeline;
    let gradient = gradient_for(&timeline.id);
    let (hero_class, cover) = match timeline.cover_image.clone() {
        Some(cover) => {
            let src = format!("/media/{}", cover.key);
            let image = view! {
                <img
                    class="hero__cover"
                    src=src
                    width=cover.width.to_string()
                    height=cover.height.to_string()
                    alt=""
                    loading="eager"
                    fetchpriority="high"
                />
            };
            ("hero hero--cover", Some(image))
        }
        None => ("hero hero--gradient", None),
    };
    let credit = timeline
        .cover_image
        .as_ref()
        .and_then(|cover| cover.credit.clone())
        .map(|credit| view! { <p class="hero__credit">{credit}</p> });
    let eyebrow = page
        .collection_title
        .unwrap_or_else(|| "Timeline".to_owned());
    let byline = page.owner_name.unwrap_or_else(|| timeline.owner_id.clone());

    view! {
        <header class=hero_class style=gradient.style() data-gradient=gradient.hue.to_string()>
            {cover}
            <div class="hero__scrim" aria-hidden="true"></div>
            <div class="hero__inner">
                <p class="hero__eyebrow">{eyebrow}</p>
                <h1 class="hero__title">{timeline.title.clone()}</h1>
                <p class="hero__deck">{timeline.description.clone()}</p>
                <div class="hero__meta">
                    <VoteCounts
                        endpoint=format!("/api/timeline/{}/vote", timeline.id)
                        target=timeline.id.clone()
                        up=timeline.upvotes
                        down=timeline.downvotes
                        label="Timeline rating"
                    />
                    <span class="hero__byline">"By " {byline}</span>
                </div>
                {credit}
            </div>
        </header>
    }
}

/// One Event on the spine. Counts are baked from the item for first paint.
#[component]
fn EventCard(timeline_id: String, event: Event) -> impl IntoView {
    let featured = event.emphasis == Emphasis::Featured;
    let card_class = if featured {
        "spine__card spine__card--featured"
    } else {
        "spine__card"
    };
    let sources = event
        .sources
        .iter()
        .map(|url| {
            let label = source_label(url);
            if is_http_source(url) {
                view! {
                    <li>
                        <a
                            class="source-chip"
                            href=url.clone()
                            rel="noopener noreferrer"
                            target="_blank"
                        >
                            {label}
                        </a>
                    </li>
                }
                .into_any()
            } else {
                view! {
                    <li>
                        <span class="source-chip source-chip--text">{label}</span>
                    </li>
                }
                .into_any()
            }
        })
        .collect_view();
    let suggest_href = format!(
        "/api/timeline/{timeline_id}/event/{}/suggest",
        event.event_id
    );

    view! {
        <li class=card_class data-event-id=event.event_id.clone()>
            <article class="event">
                <time class="event__date" datetime=event.date.clone()>
                    {event.date.clone()}
                </time>
                <h2 class="event__title">{event.title.clone()}</h2>
                <p class="event__body">{event.description.clone()}</p>
                <ul class="sources" aria-label="Sources">
                    {sources}
                </ul>
                <footer class="event__footer">
                    <VoteCounts
                        endpoint=format!(
                            "/api/timeline/{timeline_id}/event/{}/vote",
                            event.event_id
                        )
                        target=event.event_id.clone()
                        up=event.upvotes
                        down=event.downvotes
                        label="Event accuracy"
                    />
                    <a class="suggest" href=suggest_href>"Suggest an edit"</a>
                </footer>
            </article>
        </li>
    }
}

/// Baked up/down counts with HTMX vote buttons. The static page already has
/// real numbers for first paint and crawlers; HTMX posts the vote and swaps the
/// widget back with the API's fresh fragment. The markup matches the API's
/// [`vote`] fragment exactly so the first paint and a client-side refresh never
/// drift apart.
#[component]
fn VoteCounts(
    endpoint: String,
    target: String,
    up: i64,
    down: i64,
    label: &'static str,
) -> impl IntoView {
    let up_text = up.to_string();
    let down_text = down.to_string();
    let up_vals = r#"{"direction":"up"}"#;
    let down_vals = r#"{"direction":"down"}"#;
    let clear_vals = r#"{"direction":"none"}"#;

    view! {
        <div
            class="vote"
            data-vote-target=target
            data-up=up_text.clone()
            data-down=down_text.clone()
            aria-label=label
            hx-target="this"
            hx-swap="outerHTML"
        >
            <button
                class="vote__btn vote__btn--up"
                hx-post=endpoint.clone()
                hx-vals=up_vals
                aria-label="Upvote"
            >
                "▲"
            </button>
            <span class="vote__count vote__count--up">
                <span class="vote__number">{up_text.clone()}</span>
            </span>
            <button
                class="vote__btn vote__btn--down"
                hx-post=endpoint.clone()
                hx-vals=down_vals
                aria-label="Downvote"
            >
                "▼"
            </button>
            <span class="vote__count vote__count--down">
                <span class="vote__number">{down_text.clone()}</span>
            </span>
            <button
                class="vote__btn vote__btn--clear"
                hx-post=endpoint.clone()
                hx-vals=clear_vals
                aria-label="Clear vote"
            >
                "×"
            </button>
            <span class="vote__label">{label}</span>
        </div>
    }
}

/// Minimal shared shell for Home and Collection. Issue 12 owns their full
/// browsing surface; the publish path only needs them to be real, uploadable
/// documents so `/` and `/collection/<id>` are part of the re-render set.
const INDEX_STYLES: &str = r#"
:root {
  --paper: #faf8f4;
  --ink: #17171a;
  --muted: #5c5a60;
  --line: #ddd6c9;
  --accent: #a63a23;
  --serif: Georgia, "Iowan Old Style", "Times New Roman", serif;
  --sans: "Segoe UI", system-ui, -apple-system, sans-serif;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--paper);
  color: var(--ink);
  font-family: var(--sans);
  line-height: 1.6;
}
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
.index__title {
  font-family: var(--serif);
  font-size: 1.5rem;
  color: var(--ink);
  text-decoration: none;
}
.index__title:hover { color: var(--accent); }
.index__deck { margin: 0.4rem 0 0; color: #2a292e; }
.index__meta {
  margin: 0.35rem 0 0;
  font-size: 0.78rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}
.index__back { margin-top: 2rem; }
.index__back a { color: var(--muted); }
"#;

/// Inline so a page is one uploadable file. No web fonts, no images besides the
/// owner's cover, no transitions, no overlays on the reading column.
const STYLES: &str = r#"
:root {
  --paper: #faf8f4;
  --ink: #17171a;
  --muted: #5c5a60;
  --line: #ddd6c9;
  --accent: #a63a23;
  --serif: Georgia, "Iowan Old Style", "Times New Roman", serif;
  --sans: "Segoe UI", system-ui, -apple-system, sans-serif;
}
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
.hero__credit {
  margin: 1rem 0 0;
  font-size: 0.76rem;
  color: rgba(255, 255, 255, 0.6);
}
.vote {
  display: inline-flex;
  align-items: center;
  gap: 0.65rem;
  font-size: 0.82rem;
  color: var(--muted);
}
.hero .vote { color: rgba(255, 255, 255, 0.9); }
.vote__count {
  display: inline-flex;
  align-items: baseline;
  gap: 0.28rem;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}
.vote__count--up { color: #2f6b46; }
.vote__count--down { color: #9a3220; }
.hero .vote__count--up { color: #9fe0b4; }
.hero .vote__count--down { color: #ffb9a6; }
.vote__number { font-size: 0.95rem; }
.vote__btn {
  padding: 0;
  border: 0;
  background: none;
  color: inherit;
  font: inherit;
  font-size: 0.85rem;
  line-height: 1;
  cursor: pointer;
}
.vote__btn--up { color: #2f6b46; }
.vote__btn--down { color: #9a3220; }
.hero .vote__btn--up { color: #9fe0b4; }
.hero .vote__btn--down { color: #ffb9a6; }
.vote__btn--clear { color: var(--muted); font-size: 0.72rem; opacity: 0.7; }
.vote__btn:hover { opacity: 0.7; }
.vote__label {
  font-size: 0.72rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  opacity: 0.75;
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
.spine__card--featured {
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(166, 58, 35, 0.18);
}
.spine__card--featured::before {
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
.missing { padding: 4rem 1.5rem; font-family: var(--serif); }
@media (max-width: 640px) {
  .hero__inner { padding: 2.25rem 1.15rem 2.5rem; }
  .reading { margin-top: -1.25rem; padding: 0 1rem; }
  .spine { padding-left: 1.35rem; }
  .spine__card, .spine__add { margin-left: -1.35rem; }
  .spine__card { padding: 1.2rem 1.15rem; }
  .spine__card::before { left: -1.76rem; }
  .add-pill--end { margin-left: 1.35rem; }
}
"#;
