//! Single-table DynamoDB data layer.
//!
//! PRD §6 is the source of truth: every item type carries an `itemType`
//! discriminator and uses the key layouts defined in [`keys`]. Timestamps are
//! fixed-width UTC ISO-8601 so sort keys order lexicographically.

pub mod browse;
pub mod item;
pub mod keys;
pub mod model;
pub mod read;
pub mod timestamp;

#[cfg(feature = "fixture")]
pub mod fixture;

pub use timestamp::{Timestamp, TimestampError};
