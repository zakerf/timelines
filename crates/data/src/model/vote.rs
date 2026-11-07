//! VOTE items (PRD §6).

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_i64, get_string,
    number, string,
};
use crate::keys;

/// `+1` or `-1`, stored as a number so counter arithmetic needs no mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoteDirection {
    Up,
    Down,
}

impl VoteDirection {
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Up => 1,
            Self::Down => -1,
        }
    }

    pub fn from_i64(value: i64) -> Result<Self, i64> {
        match value {
            1 => Ok(Self::Up),
            -1 => Ok(Self::Down),
            other => Err(other),
        }
    }
}

/// A vote at Timeline level (`event_id = None`) or Event level.
///
/// All votes held by one account live in the `VOTE#<accountId>` partition, so
/// one query returns the viewer's own-vote state for a whole page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    pub account_id: String,
    pub timeline_id: String,
    pub event_id: Option<String>,
    pub direction: VoteDirection,
    pub created_at: Timestamp,
}

impl DynamoItem for Vote {
    const ITEM_TYPE: ItemType = ItemType::Vote;

    fn pk(&self) -> String {
        keys::vote_pk(&self.account_id)
    }

    fn sk(&self) -> String {
        match &self.event_id {
            Some(event_id) => keys::event_vote_sk(&self.timeline_id, event_id),
            None => keys::timeline_vote_sk(&self.timeline_id),
        }
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("direction".to_owned(), number(self.direction.as_i64()));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let (timeline_id, event_id) = keys::strip_vote_sk(&get_string(&item, "SK")?)?;
        let direction_value = get_i64(&item, "direction")?;
        let direction =
            VoteDirection::from_i64(direction_value).map_err(|_| DecodeError::InvalidValue {
                attribute: "direction",
                value: direction_value.to_string(),
            })?;
        Ok(Self {
            account_id: keys::strip_vote_pk(&get_string(&item, "PK")?)?,
            timeline_id,
            event_id,
            direction,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
        })
    }
}
