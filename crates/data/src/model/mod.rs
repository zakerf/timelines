//! Typed item model.
//!
//! One struct per `itemType` in PRD §6. Each implements
//! [`crate::item::DynamoItem`] so serialization and key construction live with
//! the type they belong to.

mod account;
mod collection;
mod event;
mod ratelimit;
mod report;
mod session;
mod suggestcount;
mod suggestion;
mod timeline;
mod vote;

pub use account::Account;
pub use collection::Collection;
pub use event::{Emphasis, Event};
pub use ratelimit::{RateLimit, RateLimitPartition};
pub use report::{Report, ReportTargetType};
pub use session::Session;
pub use suggestcount::SuggestCount;
pub use suggestion::{
    AppendPayload, EditPayload, Suggestion, SuggestionKind, SuggestionPayload, SuggestionState,
};
pub use timeline::{CoverImage, Timeline};
pub use vote::{Vote, VoteDirection};
