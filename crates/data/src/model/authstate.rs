//! Short-lived OIDC authorization-code state items.
//!
//! The PKCE `code_verifier` must be stored server-side: the token exchange
//! happens in the API process, not the browser, so the verifier can never be a
//! URL or cookie value the client controls. One row per login attempt, keyed by
//! the opaque `state` the browser round-trips, with an `expiresAt` TTL.

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_i64, get_string,
    number, string,
};
use crate::keys;

/// A pending authorization-code exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthState {
    /// Opaque CSRF/PKCE state echoed by the IdP.
    pub state: String,
    /// PKCE verifier (RFC 7636), never sent to the browser.
    pub code_verifier: String,
    /// The exact redirect URI used in `/oauth2/authorize`.
    pub redirect_uri: String,
    /// When the attempt was created.
    pub created_at: Timestamp,
    /// Epoch seconds for the `expiresAt` TTL attribute.
    pub expires_at: i64,
}

impl DynamoItem for AuthState {
    const ITEM_TYPE: ItemType = ItemType::AuthState;

    fn pk(&self) -> String {
        keys::auth_state_pk(&self.state)
    }

    fn sk(&self) -> String {
        keys::AUTH_STATE_SK.to_owned()
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("codeVerifier".to_owned(), string(&self.code_verifier));
        item.insert("redirectUri".to_owned(), string(&self.redirect_uri));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item.insert("expiresAt".to_owned(), number(self.expires_at));
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        Ok(Self {
            state: keys::strip_auth_state_pk(&get_string(&item, "PK")?)?,
            code_verifier: get_string(&item, "codeVerifier")?,
            redirect_uri: get_string(&item, "redirectUri")?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
            expires_at: get_i64(&item, "expiresAt")?,
        })
    }
}
