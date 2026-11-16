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
    Site, TimelinePage, author_name, event_count_label, event_span_label, gradient_for,
    group_timelines_by_story_year, human_date, is_http_source, source_labels, updated_label,
};
use crate::shell::{DocumentShell, INDEX_CSS, NOTICE_CSS, READ_CSS};
use crate::ssg::{APP_ASSET_PATH, HTMX_ASSET_PATH};

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
    // `/` and `/timelines` have no params, but a static route must still declare
    // its (empty) prerender list or the generator skips it.
    let home_route = StaticRoute::new().prerender_params(|| async { StaticParamsMap::new() });
    let timelines_route = StaticRoute::new().prerender_params(|| async { StaticParamsMap::new() });

    view! {
        <Router>
            <Routes fallback=|| view! {
                <DocumentShell
                    title="Page not found"
                    noindex=true
                    styles=&[NOTICE_CSS]
                >
                    {not_found_body("The address may be wrong, or this page may have been unpublished or deleted.")}
                </DocumentShell>
            }>
                <Route path=path!("/") view=HomeRoute ssr=SsrMode::Static(home_route) />
                <Route
                    path=path!("/timelines")
                    view=AllTimelinesRoute
                    ssr=SsrMode::Static(timelines_route)
                />
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

/// Most recently updated first, id as the tie-break. Home's preview and the
/// full `/timelines` list share this order, so "see all" just removes the cut.
fn newest_first(pages: &mut [TimelinePage]) {
    pages.sort_by(|left, right| {
        right
            .timeline
            .updated_at
            .cmp(&left.timeline.updated_at)
            .then_with(|| left.timeline.id.cmp(&right.timeline.id))
    });
}

/// Home: recently updated Timelines plus entry points into the top-level
/// Collections.
#[component]
fn HomeRoute() -> impl IntoView {
    let site = expect_context::<Site>();

    let mut recent = site.pages.clone();
    newest_first(&mut recent);
    recent.truncate(HOME_PREVIEW_LEN);
    let recent_entries = recent
        .iter()
        .map(|page| view! { <TimelineEntry page=page.clone() /> })
        .collect_view();
    // Never an empty `<ol>`: with nothing to preview the section just carries
    // the way to the full list. (Home's zero-Timeline message belongs to
    // ticket 05's empty state.)
    let recent_list =
        (!recent.is_empty()).then(|| view! { <ol class="index__list">{recent_entries}</ol> });

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

    // A fresh environment has no Timelines. That is a valid site: Home says so
    // and points at the way to start one, instead of failing the render.
    let recent_section = if recent.is_empty() {
        view! {
            <section class="index__section">
                <h2 class="index__section-title">"No Timelines yet"</h2>
                <p class="index__empty">
                    "Nothing has been published yet. A Timeline starts with one dated Event."
                </p>
                <p class="index__cta">
                    <a href="/api/timeline">"Publish the first Timeline"</a>
                </p>
                <p class="index__more"><a href="/timelines">"See all timelines"</a></p>
            </section>
        }
        .into_any()
    } else {
        view! {
            <section class="index__section">
                <h2 class="index__section-title">"Recently updated"</h2>
                {recent_list}
                <p class="index__more"><a href="/timelines">"See all timelines"</a></p>
            </section>
        }
        .into_any()
    };

    view! {
        <DocumentShell title="Timelines" canonical="/" styles=&[INDEX_CSS]>
            <header class="index__hero">
                <p class="index__eyebrow">"Home"</p>
                <h1 class="index__heading">"Timelines"</h1>
                <p class="index__lede">"Browse collections or the Timelines updated most recently."</p>
            </header>
            <main class="index">
                {collections_section}
                {recent_section}
            </main>
        </DocumentShell>
    }
}

/// How many Timelines Home previews before pointing at the full list.
const HOME_PREVIEW_LEN: usize = 12;

/// `/timelines`: every Timeline, in Home's order, with no truncation. This is
/// where Home's "See all timelines" link lands, and it is the honest full index
/// even when the preview was not cut short.
#[component]
fn AllTimelinesRoute() -> impl IntoView {
    let site = expect_context::<Site>();

    let mut pages = site.pages.clone();
    newest_first(&mut pages);
    let entries = pages
        .iter()
        .map(|page| view! { <TimelineEntry page=page.clone() /> })
        .collect_view();
    let body = if pages.is_empty() {
        view! {
            <p class="index__empty">
                "No Timelines yet. "
                <a href="/api/timeline">"Publish the first Timeline"</a>
            </p>
        }
        .into_any()
    } else {
        view! { <ol class="index__list">{entries}</ol> }.into_any()
    };

    view! {
        <DocumentShell title="All Timelines" canonical="/timelines" styles=&[INDEX_CSS]>
            <header class="index__hero">
                <p class="index__eyebrow">"Timelines"</p>
                <h1 class="index__heading">"All Timelines"</h1>
                <p class="index__lede">"Every Timeline on the site, most recently updated first."</p>
            </header>
            <main class="index">
                <section class="index__section">
                    <h2 class="index__section-title">"Every timeline"</h2>
                    {body}
                </section>
                <p class="index__back"><a href="/">"Back to Home"</a></p>
            </main>
        </DocumentShell>
    }
}

/// Collection: its child collections and its directly assigned Timelines,
/// grouped by update year. Both come from the same GSI2 partition.
///
/// A missing COLLECTION row is a not-found, not a page named after the id: the
/// id in a Timeline's `collectionId` is a reference, and a reference without a
/// row has nothing to show. Its owner actions (rename, re-describe, nest,
/// delete, membership) ship hidden and are revealed by `/assets/app.js` after
/// `GET /api/collections/{id}/access`.
#[component]
fn CollectionRoute() -> impl IntoView {
    let params = use_params_map();
    let site = expect_context::<Site>();
    let id = params.with(|params| params.get("id").unwrap_or_default());

    let Some(collection) = site
        .collections
        .iter()
        .find(|collection| collection.id == id)
        .cloned()
    else {
        return view! {
            <DocumentShell
                title="Collection not found"
                noindex=true
                styles=&[NOTICE_CSS]
            >
                {not_found_body("Collection not found. It may have been deleted, or a Timeline may still name it.")}
            </DocumentShell>
        }
        .into_any();
    };

    let title = collection.title.clone();
    let description = collection.description.clone();
    let parent = collection
        .parent_collection_id
        .as_deref()
        .and_then(|parent_id| {
            site.collections
                .iter()
                .find(|candidate| candidate.id == parent_id)
                .cloned()
        });
    let parent_link = parent.map(|parent| {
        let href = format!("/collection/{}", parent.id);
        view! {
            <span class="index__parent">
                " · part of "<a href=href>{parent.title.clone()}</a>
            </span>
        }
    });

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
    let groups = group_timelines_by_story_year(pages.clone());
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
        // An empty Collection says which Collection it is and what to do next;
        // it never renders an empty list.
        let message = format!("No Timelines in {title} yet.");
        view! {
            <p class="index__empty">
                {message}
                " "
                <a href="/api/timeline">"Publish a Timeline"</a>
            </p>
        }
        .into_any()
    } else {
        timeline_sections.into_any()
    };

    // --- Owner actions -----------------------------------------------------
    //
    // Everything here is markup the static page can carry: the Collection list
    // is public, so the nest and add-Timeline pickers can be built now; each
    // write is still refused unless the caller owns what it touches.
    let manage_href = format!("/api/collections/{}", collection.id);
    let manage_collection_id = collection.id.clone();
    let manage_title = collection.title.clone();
    let manage_description = collection.description.clone();

    let parent_options = site
        .collections
        .iter()
        .filter(|candidate| {
            candidate.id != collection.id
                && !collection_is_within(&site, &candidate.id, &collection.id)
        })
        .map(|candidate| {
            let selected =
                collection.parent_collection_id.as_deref() == Some(candidate.id.as_str());
            view! {
                <option value=candidate.id.clone() selected=selected>
                    {candidate.title.clone()}
                </option>
            }
        })
        .collect_view();

    let member_items = pages
        .iter()
        .map(|page| {
            let timeline_id = page.timeline.id.clone();
            let label = page.timeline.title.clone();
            view! {
                <li class="manage__member">
                    <span>{label}</span>
                    <form method="post" action=format!("{manage_href}/membership")>
                        <input type="hidden" name="action" value="remove" />
                        <input type="hidden" name="timelineId" value=timeline_id />
                        <button type="submit" class="manage__link">"Remove"</button>
                    </form>
                </li>
            }
        })
        .collect_view();
    let member_list = (!pages.is_empty()).then(|| {
        view! {
            <div class="manage__members">
                <h3 class="manage__subtitle">"Timelines in this collection"</h3>
                <ul class="manage__list">{member_items}</ul>
            </div>
        }
    });

    let candidates: Vec<_> = site
        .pages
        .iter()
        .filter(|page| page.timeline.collection_id.as_deref() != Some(collection.id.as_str()))
        .collect();
    let candidate_options = candidates
        .iter()
        .map(|page| {
            view! {
                <option value=page.timeline.id.clone()>
                    {page.timeline.title.clone()}
                </option>
            }
        })
        .collect_view();
    let add_form = if candidates.is_empty() {
        view! { <p class="help">"Every Timeline on the site is already in this collection."</p> }
            .into_any()
    } else {
        view! {
            <form class="manage__form" method="post" action=format!("{manage_href}/membership")>
                <label for="add-timeline">"Add a Timeline"</label>
                <select id="add-timeline" name="timelineId">{candidate_options}</select>
                <button type="submit">"Add to this collection"</button>
            </form>
        }
        .into_any()
    };
    let delete_confirm = format!("Delete the empty collection \"{manage_title}\"?");

    view! {
        <DocumentShell
            title=format!("{title} - collection")
            description=description.clone()
            canonical=format!("/collection/{}", collection.id)
            styles=&[INDEX_CSS]
        >
            <header class="index__hero">
                <p class="index__eyebrow">"Collection"{parent_link}</p>
                <h1 class="index__heading">{title}</h1>
                <p class="index__lede">{description}</p>
            </header>
            <main class="index">
                <section
                    class="manage"
                    data-owner-collection=true
                    data-collection-id=manage_collection_id
                    hidden=true
                    aria-labelledby="manage-title"
                >
                    <h2 class="manage__title" id="manage-title">"Manage this collection"</h2>
                    <form class="manage__form" method="post" action=manage_href.clone()>
                        <input type="hidden" name="action" value="rename" />
                        <label for="collection-title">"Title"</label>
                        <input id="collection-title" name="title" required=true value=manage_title />
                        <label for="collection-description">"Description"</label>
                        <textarea id="collection-description" name="description">{manage_description}</textarea>
                        <label for="collection-parent">"Nested under"</label>
                        <select id="collection-parent" name="parentCollectionId">
                            <option value="">"Top level"</option>
                            {parent_options}
                        </select>
                        <button type="submit">"Save changes"</button>
                    </form>
                    {member_list}
                    {add_form}
                    <form class="manage__form manage__danger-zone" method="post" action=manage_href.clone()>
                        <input type="hidden" name="action" value="delete" />
                        <button type="submit" class="manage__danger" data-confirm=delete_confirm>
                            "Delete this empty collection"
                        </button>
                    </form>
                </section>
                {children_section}
                <section class="index__section">
                    <h2 class="index__section-title">"Timelines"</h2>
                    {timeline_body}
                </section>
                <p class="index__back"><a href="/">"Back to all Timelines"</a></p>
            </main>
        </DocumentShell>
    }
    .into_any()
}

/// The Timeline as schema.org `Article` JSON-LD: what a crawler needs to show
/// the page as a story rather than an untitled document.
fn timeline_json_ld(page: &TimelinePage) -> String {
    let origin = crate::shell::site_origin();
    let url = format!("{origin}/timeline/{}", page.timeline.id);
    let image = match page.timeline.cover_image.as_ref() {
        Some(cover) => format!("{origin}/media/{}", cover.key),
        None => format!("{origin}{}", crate::shell::DEFAULT_OG_IMAGE),
    };
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Article",
        "headline": page.timeline.title,
        "description": page.timeline.description,
        "datePublished": page.timeline.created_at.as_str(),
        "dateModified": page.timeline.updated_at.as_str(),
        "author": {
            "@type": "Person",
            "name": author_name(page.owner_name.as_deref()),
        },
        "image": image,
        "url": url,
        "mainEntityOfPage": url,
        "publisher": {
            "@type": "Organization",
            "name": crate::shell::SITE_NAME,
        },
        "isPartOf": {
            "@type": "WebSite",
            "name": crate::shell::SITE_NAME,
            "url": format!("{origin}/"),
        },
    })
    .to_string()
}

/// True when `collection_id` is `ancestor_id` itself or nested anywhere under
/// it. Used to keep the nest picker from offering a cycle.
fn collection_is_within(site: &Site, collection_id: &str, ancestor_id: &str) -> bool {
    let mut current = Some(collection_id.to_owned());
    for _ in 0..32 {
        let Some(id) = current else {
            return false;
        };
        if id == ancestor_id {
            return true;
        }
        current = site
            .collections
            .iter()
            .find(|collection| collection.id == id)
            .and_then(|collection| collection.parent_collection_id.clone());
    }
    false
}

/// One Timeline summary row on Home, a Collection page, or `/timelines`.
///
/// A reader choosing what to open gets the facts that decision needs: the cover
/// (or the id-keyed gradient), the story's date span, who wrote it, how many
/// Events it has, and when it last changed. The thumbnail is decorative — the
/// title link beside it is the accessible name.
#[component]
fn TimelineEntry(page: TimelinePage) -> impl IntoView {
    let id = page.timeline.id.clone();
    let title = page.timeline.title.clone();
    let description = page.timeline.description.clone();
    let href = format!("/timeline/{id}");
    let event_count = event_count_label(page.events.len());
    let byline = format!("By {}", author_name(page.owner_name.as_deref()));
    let updated_iso = page.timeline.updated_at.as_str().to_owned();
    let updated = updated_label(&updated_iso);
    let span = event_span_label(&page.events);

    let thumbnail = match page.timeline.cover_image.as_ref() {
        Some(cover) => view! {
            <img
                class="index__thumb"
                src=format!("/media/{}", cover.key)
                width=cover.width.to_string()
                height=cover.height.to_string()
                alt=""
                loading="lazy"
            />
        }
        .into_any(),
        None => {
            let gradient = gradient_for(&id);
            view! {
                <span
                    class="index__thumb index__thumb--gradient"
                    style=gradient.style()
                    aria-hidden="true"
                ></span>
            }
            .into_any()
        }
    };
    let span_item = span.map(|span| view! { <span class="index__span">{span}</span> });

    view! {
        <li class="index__item">
            <div class="index__row">
                {thumbnail}
                <div class="index__body">
                    <a class="index__title" href=href>{title}</a>
                    <p class="index__deck">{description}</p>
                    <p class="index__meta">
                        {span_item}
                        <span class="index__byline">{byline}</span>
                        <span class="index__count">{event_count}</span>
                        <time class="index__updated" datetime=updated_iso>{updated}</time>
                    </p>
                </div>
            </div>
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
                {not_found_body("This Timeline is not here. It may have been unpublished or deleted by its owner.")}
            </DocumentShell>
        }
        .into_any(),
    }
}

/// A complete, self-contained document: stylesheet inline, HTMX plus the
/// `/assets/app.js` behaviour script (no Leptos hydration bundle).
#[component]
fn TimelineDocument(page: TimelinePage) -> impl IntoView {
    let site = expect_context::<Site>();
    let timeline_id = page.timeline.id.clone();
    let events = page.events.clone();
    let canonical = format!("/timeline/{}", page.timeline.id);
    let og_image = match page.timeline.cover_image.as_ref() {
        Some(cover) => format!("/media/{}", cover.key),
        None => crate::shell::DEFAULT_OG_IMAGE.to_owned(),
    };
    let structured_data = timeline_json_ld(&page);
    let siblings = page
        .timeline
        .collection_id
        .as_deref()
        .map(|collection_id| site.siblings(collection_id, &timeline_id))
        .unwrap_or_default();
    let rows = events
        .iter()
        .map(|event| view! { <EventCard timeline_id=timeline_id.clone() event=event.clone() /> })
        .collect_view();
    // The crucial marker is explained where it appears, not left as a ring.
    let has_crucial = events
        .iter()
        .any(|event| event.significance == Significance::Crucial);
    let significance_legend = has_crucial.then(|| {
        view! {
            <p class="spine__legend">
                "Events marked "
                <span class="event__significance-badge">"Crucial"</span>
                " are the turning points of this story."
            </p>
        }
    });
    // Events are stored and read in date order, so there is no position to add
    // at: the one add action sits at the end. It ships hidden and owner-only;
    // `/assets/app.js` reveals it only after the ownership check.
    let add_href = format!("/api/timeline/{timeline_id}/events/new");
    let add_timeline_id = timeline_id.clone();
    let pending_timeline_id = timeline_id.clone();
    let queue_timeline_id = timeline_id.clone();
    // The public counterpart to the owner's add action: a reader who spots a
    // missing fact proposes it and the owner decides.
    let suggest_href = format!("/api/timeline/{timeline_id}/events/suggest");
    let timeline_report_endpoint = format!("/api/timeline/{timeline_id}/report");
    let reports_href = format!("/api/timeline/{timeline_id}/reports");
    let reports_timeline_id = timeline_id.clone();
    let settings_href = format!("/api/timeline/{timeline_id}/settings");
    let settings_timeline_id = timeline_id.clone();
    // The end of the page is not a dead end: the rest of this collection, and
    // the way Home. An uncollected or lone Timeline still gets the way Home.
    let related = (!siblings.is_empty()).then(|| {
        let entries = siblings
            .iter()
            .map(|sibling| {
                let href = format!("/timeline/{}", sibling.timeline.id);
                view! {
                    <li class="page-end__item">
                        <a class="page-end__link" href=href>{sibling.timeline.title.clone()}</a>
                        <span class="page-end__meta">
                            {event_count_label(sibling.events.len())}
                        </span>
                    </li>
                }
            })
            .collect_view();
        view! {
            <aside class="page-end__related" aria-labelledby="page-end-related-title">
                <h2 class="page-end__title" id="page-end-related-title">
                    "More in this collection"
                </h2>
                <ul class="page-end__list">{entries}</ul>
            </aside>
        }
    });

    view! {
        <DocumentShell
            title=page.timeline.title.clone()
            description=page.timeline.description.clone()
            canonical=canonical
            og_image=og_image
            og_type="article"
            structured_data=structured_data
            styles=&[READ_CSS]
            scripts=&[HTMX_ASSET_PATH, APP_ASSET_PATH]
        >
            <Hero page=page />
            <main class="reading">
                {significance_legend}
                <ol class="spine">{rows}</ol>
                <a
                    class="add-pill add-pill--end"
                    href=add_href
                    hidden=true
                    data-owner-only=true
                    data-timeline-id=add_timeline_id
                >
                    "Add an event"
                </a>
                <p class="reading__suggest">
                    <a class="suggest suggest--event" href=suggest_href>
                        "Missing an event? Suggest one"
                    </a>
                </p>
                <footer class="page-end">
                    {related}
                    <div class="page-end__admin">
                        <a
                            class="page-end__settings"
                            href=settings_href
                            hidden=true
                            data-owner-only=true
                            data-timeline-id=settings_timeline_id
                        >
                            "Settings"
                        </a>
                        <ReportControl
                            endpoint=timeline_report_endpoint
                            target="timeline".to_owned()
                            subject="this timeline"
                        />
                        <a
                            class="page-end__flags"
                            href=reports_href
                            hidden=true
                            data-owner-only=true
                            data-timeline-id=reports_timeline_id
                        >
                            "Reports and flags"
                        </a>
                    </div>
                    <p class="page-end__home"><a href="/">"Home"</a></p>
                </footer>
                <section
                    class="pending"
                    data-pending-suggestions=true
                    data-timeline-id=pending_timeline_id
                    aria-labelledby="pending-title"
                >
                    <h2 class="pending__title" id="pending-title">"Pending suggestions"</h2>
                    <p class="pending__empty" data-pending-empty=true>
                        "No pending suggestions right now."
                    </p>
                    <ol class="pending__list" data-pending-list=true hidden></ol>
                    <p class="pending__queue">
                        <a
                            class="pending__queue-link"
                            href="/api/suggestions/queue"
                            hidden=true
                            data-owner-only=true
                            data-timeline-id=queue_timeline_id
                        >
                            "Review pending suggestions"
                        </a>
                    </p>
                </section>
            </main>
        </DocumentShell>
    }
}

/// Full-bleed cover hero, or the id-keyed gradient when there is no cover.
///
/// This is the one `<header class="hero …">` per document: the API's cover
/// write path slices it out with `render::extract_hero` for read-your-writes.
#[component]
fn Hero(page: TimelinePage) -> impl IntoView {
    let TimelinePage {
        timeline,
        events,
        owner_name,
        collection_title,
    } = page;
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
    // The eyebrow is a navigational parent, never a generic label: a collected
    // Timeline links its Collection, an uncollected one has no eyebrow at all.
    let eyebrow = timeline.collection_id.as_ref().map(|collection_id| {
        let href = format!("/collection/{collection_id}");
        let label = collection_title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Collection".to_owned());
        view! {
            <p class="hero__eyebrow"><a href=href>{label}</a></p>
        }
    });
    // A missing ACCOUNT row never turns into the owner's id: the byline falls
    // back to "the editors" (see `page::author_name`).
    let byline = author_name(owner_name.as_deref()).to_owned();
    let updated_iso = timeline.updated_at.as_str().to_owned();
    let updated = updated_label(&updated_iso);
    let event_count = event_count_label(events.len());

    view! {
        <header class=hero_class style=gradient.style() data-gradient=gradient.hue.to_string()>
            {cover}
            <div class="hero__scrim" aria-hidden="true"></div>
            <div class="hero__inner">
                {eyebrow}
                <h1 class="hero__title">{timeline.title.clone()}</h1>
                <p class="hero__deck">{timeline.description.clone()}</p>
                <div class="hero__meta">
                    <VoteCounts
                        endpoint=format!("/api/timeline/{}/vote", timeline.id)
                        target=timeline.id.clone()
                        up=timeline.upvotes
                        down=timeline.downvotes
                        label=crate::vote::TIMELINE_LABEL
                    />
                    <span class="hero__byline">"By " {byline}</span>
                    <time class="hero__updated" datetime=updated_iso>{updated}</time>
                    <span class="hero__count">{event_count}</span>
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
    // A stable, citable address for this fact: `#event-<id>`.
    let anchor_id = format!("event-{}", event.event_id);
    let anchor_href = format!("#{anchor_id}");
    let date_iso = event.date.clone();
    let date_text = human_date(&event.date);
    let labels = source_labels(&event.sources);
    let sources = event
        .sources
        .iter()
        .zip(labels)
        .map(|(url, label)| {
            if is_http_source(url) {
                view! {
                    <li>
                        <a
                            class="source-chip"
                            href=url.clone()
                            rel="noopener noreferrer"
                            target="_blank"
                            title=url.clone()
                        >
                            {label}
                            <span class="source-chip__new-tab" aria-hidden="true">" ↗"</span>
                            <span class="sr-only">" (opens in a new tab)"</span>
                        </a>
                    </li>
                }
                .into_any()
            } else {
                view! {
                    <li>
                        <span class="source-chip source-chip--text" title=url.clone()>
                            {label}
                        </span>
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
    let significance_marker = crucial.then(|| {
        view! {
            <p class="event__significance">
                <span class="event__significance-badge">"Crucial"</span>
                "A turning point in this story"
            </p>
        }
    });
    let report_endpoint = format!(
        "/api/timeline/{timeline_id}/event/{}/report",
        event.event_id
    );
    let report_target = format!("event-{}", event.event_id);

    view! {
        <li class=card_class id=anchor_id data-event-id=event.event_id.clone()>
            <article class="event">
                <a class="event__anchor" href=anchor_href aria-label="Link to this event">
                    "¶"
                </a>
                <time class="event__date" datetime=date_iso>{date_text}</time>
                <h2 class="event__title">{event.title.clone()}</h2>
                {significance_marker}
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
                        label=crate::vote::EVENT_LABEL
                    />
                    <a class="suggest" href=suggest_href>"Suggest an edit"</a>
                    <ReportControl
                        endpoint=report_endpoint
                        target=report_target
                        subject="this event"
                    />
                </footer>
            </article>
        </li>
    }
}

/// Baked counts with HTMX vote buttons. The static page already has real
/// numbers for first paint and crawlers; HTMX posts the vote and swaps the
/// widget for the API's fresh fragment.
///
/// The markup comes from [`crate::vote::widget_html`], the same builder the API
/// fragment uses, so the first paint and a client-side refresh cannot drift
/// apart. The builder emits the whole widget, so it is embedded as raw HTML
/// (`inner_html`); everything interpolated is escaped by the builder itself.
/// The wrapper only exists because a Leptos view cannot return a bare HTML
/// fragment; `READ_CSS` gives it `display: contents`, so it has no box and no
/// effect on the layout or on the widget's semantics.
#[component]
fn VoteCounts(
    endpoint: String,
    target: String,
    up: i64,
    down: i64,
    label: &'static str,
) -> impl IntoView {
    let html = crate::vote::widget_html(&crate::vote::VoteWidget {
        endpoint: &endpoint,
        target: &target,
        upvotes: up,
        downvotes: down,
        label,
        // The static page cannot know the reader, so it paints the neutral
        // state; `assets/app.js` asks for the own votes and repaints.
        my_vote: None,
    });

    view! { <div class="vote-slot" inner_html=html></div> }
}

/// The report control, built by [`crate::report::widget_html`] so a Timeline, an
/// Event, and a pending Suggestion all offer the same two-step action. The
/// wrapper only exists because a view cannot return a bare fragment; `READ_CSS`
/// gives it `display: contents`.
#[component]
fn ReportControl(endpoint: String, target: String, subject: &'static str) -> impl IntoView {
    let html = crate::report::widget_html(&crate::report::ReportWidget {
        endpoint: &endpoint,
        target: &target,
        subject,
    });

    view! { <div class="report-slot" inner_html=html></div> }
}
