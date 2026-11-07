//! REPORT items (PRD §8).
//!
//! One item per account per target: existence is the idempotency key. Reports
//! are anonymous to the owner, so no reporter identity is copied onto the
//! target.

use std::fmt;
use std::str::FromStr;

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_string, string,
};
use crate::keys;

/// What a report points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportTargetType {
    Timeline,
    Event,
    Suggestion,
}

impl ReportTargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeline => "TIMELINE",
            Self::Event => "EVENT",
            Self::Suggestion => "SUGGESTION",
        }
    }
}

impl fmt::Display for ReportTargetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReportTargetType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "TIMELINE" => Ok(Self::Timeline),
            "EVENT" => Ok(Self::Event),
            "SUGGESTION" => Ok(Self::Suggestion),
            other => Err(format!("unknown report target type {other:?}")),
        }
    }
}

/// A logged-in user's flag on a Timeline, Event, or Suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub target_type: ReportTargetType,
    pub target_id: String,
    pub account_id: String,
    pub created_at: Timestamp,
}

impl DynamoItem for Report {
    const ITEM_TYPE: ItemType = ItemType::Report;

    fn pk(&self) -> String {
        keys::report_pk(self.target_type, &self.target_id)
    }

    fn sk(&self) -> String {
        keys::report_sk(&self.account_id)
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("targetType".to_owned(), string(self.target_type.as_str()));
        item.insert("targetId".to_owned(), string(&self.target_id));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let (target_type, target_id) = keys::strip_report_pk(&get_string(&item, "PK")?)?;
        Ok(Self {
            target_type,
            target_id,
            account_id: keys::strip_report_sk(&get_string(&item, "SK")?)?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
        })
    }
}
