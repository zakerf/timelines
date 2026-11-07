//! DynamoDB persistence for the auth seam.
//!
//! Three item types are involved: the short-lived `AUTHSTATE#<state>` handshake
//! row, the `ACCOUNT#<sub>` profile (which also holds the encrypted refresh
//! token), and the `SESSION#<id>` row the cookie points at.

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use data::item::DynamoItem;
use data::keys;
use data::model::{Account, AuthState, Session};

use super::error::AuthError;

fn s(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn decode<T: DynamoItem>(
    item: std::collections::HashMap<String, AttributeValue>,
) -> Result<T, AuthError> {
    T::from_item(item).map_err(|error| {
        AuthError::internal(format!("could not decode {} item: {error}", T::ITEM_TYPE))
    })
}

async fn send<T>(
    outcome: Result<T, aws_sdk_dynamodb::error::SdkError<impl std::fmt::Debug>>,
    operation: &str,
) -> Result<T, AuthError> {
    outcome.map_err(|error| AuthError::internal(format!("{operation} failed: {error:?}")))
}

/// Persist the server half of a PKCE handshake.
pub async fn put_auth_state(
    client: &Client,
    table: &str,
    state: &AuthState,
) -> Result<(), AuthError> {
    send(
        client
            .put_item()
            .table_name(table)
            .set_item(Some(state.to_item()))
            .send()
            .await,
        "PutItem(AUTHSTATE)",
    )
    .await?;
    Ok(())
}

/// Atomically fetch and remove a handshake row, so a replayed callback finds
/// nothing. `None` means the `state` was never issued or already used.
pub async fn take_auth_state(
    client: &Client,
    table: &str,
    state_id: &str,
) -> Result<Option<AuthState>, AuthError> {
    let response = send(
        client
            .delete_item()
            .table_name(table)
            .key("PK", s(keys::auth_state_pk(state_id)))
            .key("SK", s(keys::AUTH_STATE_SK))
            .return_values(ReturnValue::AllOld)
            .send()
            .await,
        "DeleteItem(AUTHSTATE)",
    )
    .await?;

    match response.attributes {
        Some(attributes) => decode(attributes).map(Some),
        None => Ok(None),
    }
}

/// Insert or refresh the account row on login. `createdAt` is preserved on
/// update (`if_not_exists`); display name and `lastSeenAt` track the latest
/// successful login. An encrypted refresh token is written only when the
/// provider returned one.
pub async fn upsert_account(
    client: &Client,
    table: &str,
    account: &Account,
) -> Result<(), AuthError> {
    let mut update_expression = String::from(
        "SET itemType = :item_type, displayName = :display_name, lastSeenAt = :last_seen, \
         createdAt = if_not_exists(createdAt, :created_at)",
    );
    let mut request = client
        .update_item()
        .table_name(table)
        .key("PK", s(keys::account_pk(&account.sub)))
        .key("SK", s(keys::ACCOUNT_SK))
        .expression_attribute_values(":item_type", s("ACCOUNT"))
        .expression_attribute_values(":display_name", s(&account.display_name))
        .expression_attribute_values(":last_seen", s(account.last_seen_at.as_str()))
        .expression_attribute_values(":created_at", s(account.created_at.as_str()));

    if let Some(ciphertext) = &account.refresh_token_ciphertext {
        update_expression.push_str(", refreshTokenCiphertext = :refresh");
        request = request.expression_attribute_values(":refresh", s(ciphertext));
    }

    send(
        request.update_expression(update_expression).send().await,
        "UpdateItem(ACCOUNT)",
    )
    .await?;
    Ok(())
}

pub async fn get_account(
    client: &Client,
    table: &str,
    sub: &str,
) -> Result<Option<Account>, AuthError> {
    let response = send(
        client
            .get_item()
            .table_name(table)
            .key("PK", s(keys::account_pk(sub)))
            .key("SK", s(keys::ACCOUNT_SK))
            .send()
            .await,
        "GetItem(ACCOUNT)",
    )
    .await?;

    match response.item {
        Some(item) => decode(item).map(Some),
        None => Ok(None),
    }
}

/// Read just the encrypted refresh token, if any, for revocation on logout.
pub async fn get_refresh_token_ciphertext(
    client: &Client,
    table: &str,
    sub: &str,
) -> Result<Option<String>, AuthError> {
    let response = send(
        client
            .get_item()
            .table_name(table)
            .key("PK", s(keys::account_pk(sub)))
            .key("SK", s(keys::ACCOUNT_SK))
            .send()
            .await,
        "GetItem(ACCOUNT refresh token)",
    )
    .await?;

    Ok(response
        .item
        .and_then(|item| item.get("refreshTokenCiphertext").cloned())
        .and_then(|value| match value {
            AttributeValue::S(value) => Some(value),
            _ => None,
        }))
}

pub async fn create_session(
    client: &Client,
    table: &str,
    session: &Session,
) -> Result<(), AuthError> {
    send(
        client
            .put_item()
            .table_name(table)
            .set_item(Some(session.to_item()))
            .send()
            .await,
        "PutItem(SESSION)",
    )
    .await?;
    Ok(())
}

pub async fn get_session(
    client: &Client,
    table: &str,
    session_id: &str,
) -> Result<Option<Session>, AuthError> {
    let response = send(
        client
            .get_item()
            .table_name(table)
            .key("PK", s(keys::session_pk(session_id)))
            .key("SK", s(keys::SESSION_SK))
            .send()
            .await,
        "GetItem(SESSION)",
    )
    .await?;

    match response.item {
        Some(item) => decode(item).map(Some),
        None => Ok(None),
    }
}

/// Revocation is one `DeleteItem` (PRD §7).
pub async fn delete_session(
    client: &Client,
    table: &str,
    session_id: &str,
) -> Result<(), AuthError> {
    send(
        client
            .delete_item()
            .table_name(table)
            .key("PK", s(keys::session_pk(session_id)))
            .key("SK", s(keys::SESSION_SK))
            .send()
            .await,
        "DeleteItem(SESSION)",
    )
    .await?;
    Ok(())
}

/// Build a session item with the configured TTL.
pub fn new_session(account_id: &str, ttl_seconds: i64) -> Session {
    Session {
        id: super::pkce::generate_token(),
        account_id: account_id.to_owned(),
        expires_at: data::timestamp::now_epoch_seconds() + ttl_seconds,
    }
}
