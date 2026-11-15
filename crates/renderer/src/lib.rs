//! Leptos 0.8 static-site renderer for the Timeline read page.
//!
//! One-shot flow:
//!
//! 1. Read a [`data::model::Timeline`] and its ordered [`data::model::Event`]s
//!    from DynamoDB ([`dynamo`]).
//! 2. Install the custom one-shot executor ([`init_executor`]); Leptos has no
//!    built-in executor that works for SSG, and the reason is documented in
//!    `src/executor.rs`.
//! 3. Generate the static route list, render each route to a complete HTML
//!    document, and write it with `static_file_path` ([`generate_site`]).
//!
//! Read pages ship no Leptos hydration bundle — HTMX is the one script they
//! include — which is why the renderer keeps Leptos's route list but renders
//! through the hand-rolled `render_app_async_with_context` path instead of the
//! streaming static generator, and drops the generator's resource bookkeeping
//! scripts.

// Wrapping the router + document view exceeds rustc's default query depth.
#![recursion_limit = "512"]

pub mod dynamo;

mod executor;
mod page;
pub mod shell;
mod ssg;
mod ui;

pub use executor::init_executor;
pub use page::{
    Gradient, Site, TimelineGroup, TimelinePage, gradient_for, group_timelines_by_year,
    is_http_source, source_label,
};
pub use ssg::{
    GeneratedPage, PublishedPage, generate_site, object_key, publish_routes, render_path,
};
