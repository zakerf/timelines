//! The Spine read page as Leptos views.
//!
//! One complete HTML document per Timeline: full-bleed hero (cover or the
//! deterministic gradient), oversized serif title, deck, byline, Timeline-level
//! vote count, then Event cards on a vertical spine with baked Event-level
//! counts and add/edit affordances.
//!
//! The reading column is intentionally plain: one comfortable measure, high
//! contrast, no overlays and no animation. The hero is the only grand surface.
//!
//! Every route renders its whole document through [`crate::shell`], which owns
//! the tokens, the base styles, and the page stylesheet blocks.

use leptos::prelude::*;
use leptos_router::SsrMode;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_params_map;
use leptos_router::path;
use leptos_router::static_routes::{StaticParamsMap, StaticRoute};

use data::model::{Event, Significance};

use crate::page::{
    Site, TimelinePage, gradient_for, group_timelines_by_year, is_http_source, source_label,
};
use crate::shell::{DocumentShell, INDEX_CSS, NOTICE_CSS, READ_CSS};
use crate::ssg::HTMX_ASSET_PATH;

/// The router's miss page and the "Timeline not found" branch share this body:
/// a light notice with a way home, inside the one document shell.
fn not_found_body(message: &'static str) -> impl IntoView {
    view! {
        <main>
            <p class="eyebrow">"404"</p>
            <h1>"Page not found"</h1>
            <p class="missing">{message}</p>
            <p><a href="/">"Back to Home"</a></p>
        </main>
    }
}

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
            <Routes fallback=|| view! {
                <DocumentShell
                    title="Page not found"
                    noindex=true
                    styles=&[NOTICE_CSS]
                >
                    {not_found_body("Page not found.")}
                </DocumentShell>
            }>
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
        <DocumentShell title="Timelines" styles=&[INDEX_CSS]>
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
        </DocumentShell>
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
        <DocumentShell
            title=format!("{title} - collection")
            description=description.clone()
            styles=&[INDEX_CSS]
        >
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
        </DocumentShell>
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
            <DocumentShell
                title="Timeline not found"
                noindex=true
                styles=&[NOTICE_CSS]
            >
                {not_found_body("Timeline not found.")}
            </DocumentShell>
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
        <DocumentShell
            title=format!("{} - timeline", page.timeline.title)
            description=page.timeline.description.clone()
            styles=&[READ_CSS]
            scripts=&[HTMX_ASSET_PATH]
        >
            <Hero page=page />
            <main class="reading">
                <ol class="spine">{rows}</ol>
                <a class="add-pill add-pill--end" href=end_href>
                    "+ Add an event at the end"
                </a>
            </main>
        </DocumentShell>
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
    let crucial = event.significance == Significance::Crucial;
    let card_class = if crucial {
        "spine__card spine__card--crucial"
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
