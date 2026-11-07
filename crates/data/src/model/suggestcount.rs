//! SUGGESTCOUNT items (PRD §8): the per-timeline pending cap.
//!
//! A counter, not a time window. Incremented on submit and decremented on
//! approve/reject, conditioned `< 10`.

use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_i64, get_string,
    number,
};
use crate::keys;

/// How many suggestions one account currently has pending on one Timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestCount {
    pub timeline_id: String,
    pub account_id: String,
    pub pending: i64,
}

impl DynamoItem for SuggestCount {
    const ITEM_TYPE: ItemType = ItemType::SuggestCount;

    fn pk(&self) -> String {
        keys::suggestcount_pk(&self.timeline_id)
    }

    fn sk(&self) -> String {
        keys::suggestcount_sk(&self.account_id)
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("pending".to_owned(), number(self.pending));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        Ok(Self {
            timeline_id: keys::strip_timeline_pk(&get_string(&item, "PK")?)?,
            account_id: keys::strip_suggestcount_sk(&get_string(&item, "SK")?)?,
            pending: get_i64(&item, "pending")?,
        })
    }
}
