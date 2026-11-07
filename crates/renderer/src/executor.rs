//! The one-shot SSG executor.
//!
//! Leptos has no built-in executor for a one-shot static generator:
//!
//! - Leptos routes `Send` work through [`any_spawner::Executor::spawn`]; the
//!   natural choice is `tokio::spawn`, and the generator's `tokio::fs` writes
//!   require a Tokio runtime anyway (futures' `block_on` panics with "there is
//!   no reactor running").
//! - The reactive graph and the router also call `Executor::spawn_local` for
//!   `!Send` effects. Tokio executors panic on `spawn_local` outside a
//!   `LocalSet`, so those futures go to a thread-local futures `LocalPool`.
//!
//! The `LocalPool` is deliberately never driven: static generation has no
//! browser to update, so the local effects have nothing to do. They only need
//! somewhere legal to be deposited.
//!
//! This is the ~25-line adapter the framework requires; it is installed once
//! before any Lepos API touches the reactive graph.

use std::cell::RefCell;
use std::sync::Once;

use any_spawner::{CustomExecutor, PinnedFuture, PinnedLocalFuture};

/// Custom executor: `Send` to Tokio, local to an undriven `LocalPool`.
struct SsgExecutor;

impl CustomExecutor for SsgExecutor {
    fn spawn(&self, future: PinnedFuture<()>) {
        tokio::spawn(future);
    }

    fn spawn_local(&self, future: PinnedLocalFuture<()>) {
        use futures::task::LocalSpawnExt;

        thread_local! {
            static POOL: RefCell<futures::executor::LocalPool> =
                RefCell::new(futures::executor::LocalPool::new());
            static SPAWNER: futures::executor::LocalSpawner =
                POOL.with(|pool| pool.borrow().spawner());
        }

        SPAWNER.with(|spawner| {
            spawner
                .spawn_local(future)
                .expect("local SSG tasks spawn on the thread-local LocalPool")
        });
    }

    fn poll_local(&self) {}
}

static INSTALL: Once = Once::new();

/// Install the SSG executor if this process has not done so yet.
///
/// Must be called before `generate_route_list_with_ssg` or any render: those
/// call [`any_spawner::Executor::init_tokio`] themselves only if an executor is
/// not already installed.
pub fn init_executor() {
    INSTALL.call_once(|| {
        let _ = any_spawner::Executor::init_custom_executor(SsgExecutor);
    });
}
