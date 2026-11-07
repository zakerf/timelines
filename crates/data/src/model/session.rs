//! SESSION items (PRD §7).
//!
//! The browser holds only an opaque session id; revocation is one DeleteItem.

use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_i64, get_string,
    number, string,
};
use crate::keys;

/// A server-side session. `expires_at` is epoch seconds for the `expiresAt`
/// TTL attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub account_id: String,
    pub expires_at: i64,
}

impl DynamoItem for Session {
    const ITEM_TYPE: ItemType = ItemType::Session;

    fn pk(&self) -> String {
        keys::session_pk(&self.id)
    }

    fn sk(&self) -> String {
        keys::SESSION_SK.to_owned()
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("accountId".to_owned(), string(&self.account_id));
        item.insert("expiresAt".to_owned(), number(self.expires_at));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        Ok(Self {
            id: keys::strip_session_pk(&get_string(&item, "PK")?)?,
            account_id: get_string(&item, "accountId")?,
            expires_at: get_i64(&item, "expiresAt")?,
        })
    }
}
