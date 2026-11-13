//! Single-row and single-partition reads shared by every API store.
//!
//! The caller picks the consistency: votes and reports compare what they read
//! against a conditional transaction, so they read consistently; browsing and
//! suggestion lists can tolerate an eventually consistent replica.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;

use crate::item::DynamoItem;
use crate::keys;
use crate::model::{Event, Suggestion, Timeline};

/// Read failure, kept as an opaque boxed error like [`crate::browse`].
pub type ReadResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Get one TIMELINE row.
pub async fn get_timeline(
    client: &Client,
    table: &str,
    timeline_id: &str,
    consistent: bool,
) -> ReadResult<Option<Timeline>> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .consistent_read(consistent)
        .send()
        .await
        .map_err(|error| operation("GetItem(TIMELINE)", error))?;

    match output.item {
        Some(item) => Ok(Some(
            Timeline::from_item(item).map_err(|error| decode("TIMELINE", error))?,
        )),
        None => Ok(None),
    }
}

/// Get one SUGGESTION row.
pub async fn get_suggestion(
    client: &Client,
    table: &str,
    timeline_id: &str,
    suggestion_id: &str,
    consistent: bool,
) -> ReadResult<Option<Suggestion>> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::suggestion_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::suggestion_sk(suggestion_id)))
        .consistent_read(consistent)
        .send()
        .await
        .map_err(|error| operation("GetItem(SUGGESTION)", error))?;

    match output.item {
        Some(item) => Ok(Some(
            Suggestion::from_item(item).map_err(|error| decode("SUGGESTION", error))?,
        )),
        None => Ok(None),
    }
}

/// Every EVENT in a Timeline partition, in sort-key (timestamp) order.
pub async fn list_events(
    client: &Client,
    table: &str,
    timeline_id: &str,
    consistent: bool,
) -> ReadResult<Vec<Event>> {
    let mut events = Vec::new();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(keys::event_pk(timeline_id)))
            .expression_attribute_values(
                ":prefix",
                AttributeValue::S(keys::EVENT_SK_PREFIX.to_owned()),
            )
            .consistent_read(consistent);

        if let Some(key) = start_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let output = request
            .send()
            .await
            .map_err(|error| operation("Query(EVENT)", error))?;

        for item in output.items.unwrap_or_default() {
            events.push(Event::from_item(item).map_err(|error| decode("EVENT", error))?);
        }

        match output.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => return Ok(events),
        }
    }
}

/// One EVENT by id. Event rows carry their timestamp in the sort key, so an id
/// lookup is a partition read followed by an id match; volume is tiny and the
/// read is bounded by one Timeline.
pub async fn get_event(
    client: &Client,
    table: &str,
    timeline_id: &str,
    event_id: &str,
    consistent: bool,
) -> ReadResult<Option<Event>> {
    Ok(list_events(client, table, timeline_id, consistent)
        .await?
        .into_iter()
        .find(|event| event.event_id == event_id))
}

fn operation(name: &str, error: impl Debug) -> Box<dyn Error + Send + Sync> {
    format!("{name} failed: {error:?}").into()
}

fn decode(name: &str, error: impl std::fmt::Display) -> Box<dyn Error + Send + Sync> {
    format!("decode {name}: {error}").into()
}
