//! ACCOUNT items (PRD §6).

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_opt_string,
    get_string, string,
};
use crate::keys;

/// The account row the auth seam upserts on first login. `sub` is the IdP
/// subject, so the data model stays independent of any provider's attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub sub: String,
    pub display_name: String,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    /// Opaque, KMS-encrypted refresh token. Absent until a login returns one;
    /// the plaintext token never enters the table or the browser (PRD §7).
    pub refresh_token_ciphertext: Option<String>,
}

impl DynamoItem for Account {
    const ITEM_TYPE: ItemType = ItemType::Account;

    fn pk(&self) -> String {
        keys::account_pk(&self.sub)
    }

    fn sk(&self) -> String {
        keys::ACCOUNT_SK.to_owned()
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("displayName".to_owned(), string(&self.display_name));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item.insert("lastSeenAt".to_owned(), string(self.last_seen_at.as_str()));
        if let Some(ciphertext) = &self.refresh_token_ciphertext {
            item.insert("refreshTokenCiphertext".to_owned(), string(ciphertext));
        }
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        Ok(Self {
            sub: keys::strip_account_pk(&get_string(&item, "PK")?)?,
            display_name: get_string(&item, "displayName")?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
            last_seen_at: crate::item::get_timestamp(&item, "lastSeenAt")?,
            refresh_token_ciphertext: get_opt_string(&item, "refreshTokenCiphertext")?,
        })
    }
}
