//! RATELIMIT items (PRD §8).
//!
//! Fixed-window counters checked atomically in the same transaction as the
//! write they guard. Only `sha256(clientIp + salt)` is stored for IP limits.

use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_i64, get_string,
    number,
};
use crate::keys;

/// Which party a limit is counted against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateLimitPartition {
    Account(String),
    IpHash(String),
}

impl RateLimitPartition {
    pub fn pk(&self) -> String {
        match self {
            Self::Account(account_id) => keys::ratelimit_account_pk(account_id),
            Self::IpHash(hash) => keys::ratelimit_ip_pk(hash),
        }
    }
}

/// One fixed-window counter. `expires_at` is epoch seconds for the table's
/// `expiresAt` TTL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimit {
    pub partition: RateLimitPartition,
    pub action: String,
    /// Minute (`YYYY-MM-DDTHH:MM`) or date (`YYYY-MM-DD`), per limit.
    pub window_bucket: String,
    pub count: i64,
    pub expires_at: i64,
}

impl DynamoItem for RateLimit {
    const ITEM_TYPE: ItemType = ItemType::RateLimit;

    fn pk(&self) -> String {
        self.partition.pk()
    }

    fn sk(&self) -> String {
        keys::ratelimit_sk(&self.action, &self.window_bucket)
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("count".to_owned(), number(self.count));
        item.insert("expiresAt".to_owned(), number(self.expires_at));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let (action, window_bucket) = keys::strip_ratelimit_sk(&get_string(&item, "SK")?)?;
        Ok(Self {
            partition: keys::strip_ratelimit_pk(&get_string(&item, "PK")?)?,
            action,
            window_bucket,
            count: get_i64(&item, "count")?,
            expires_at: get_i64(&item, "expiresAt")?,
        })
    }
}
