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

use crate::page::{Site, TimelinePage, gradient_for, is_http_source, source_label};

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

    view! {
        <Router>
            <Routes fallback=|| view! { <p class="missing">"Page not found."</p> }>
                <Route
                    path=path!("/timeline/:id")
                    view=TimelineRoute
                    ssr=SsrMode::Static(static_route)
                />
            </Routes>
        </Router>
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

/// A complete, self-contained document: stylesheet inline, no scripts.
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
                            href=format!(
                                "/api/timeline/{timeline_id}/events/new?after={}",
                                event.event_id,
                            )
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

/// Baked up/down counts. HTMX refreshes these later; the static page already
/// has real numbers for first paint and crawlers.
#[component]
fn VoteCounts(target: String, up: i64, down: i64, label: &'static str) -> impl IntoView {
    let up_text = up.to_string();
    let down_text = down.to_string();

    view! {
        <div
            class="vote"
            data-vote-target=target
            data-up=up_text.clone()
            data-down=down_text.clone()
            aria-label=label
        >
            <span class="vote__count vote__count--up">
                <span aria-hidden="true">"▲"</span>
                <span class="vote__number">{up_text.clone()}</span>
            </span>
            <span class="vote__count vote__count--down">
                <span aria-hidden="true">"▼"</span>
                <span class="vote__number">{down_text.clone()}</span>
            </span>
            <span class="vote__label">{label}</span>
        </div>
    }
}

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
