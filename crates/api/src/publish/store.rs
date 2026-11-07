//! DynamoDB persistence for the first mutation: create a Timeline and its
//! initial Events in one `TransactWriteItems`.
//!
//! The Timeline `Put` is conditioned on `attribute_not_exists(PK)` so a
//! generated id can never overwrite an existing Timeline. `eventCount` is
//! stamped on the Timeline row in the same transaction as the Event rows, so a
//! publish never leaves a half-written timeline (PRD §6, access pattern 8).

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{Put, TransactWriteItem};
use data::item::DynamoItem;
use data::model::{Event, Timeline};

/// Store failure, kept as a message: handlers log it and return a coarse 500.
pub type StoreError = String;

pub async fn create_timeline(
    client: &Client,
    table: &str,
    timeline: &Timeline,
    events: &[Event],
) -> Result<(), StoreError> {
    let mut writes = Vec::with_capacity(events.len() + 1);

    let timeline_put = Put::builder()
        .table_name(table)
        .set_item(Some(timeline.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .build()
        .map_err(|error| format!("could not build the TIMELINE put: {error}"))?;
    writes.push(TransactWriteItem::builder().put(timeline_put).build());

    for event in events {
        let event_put = Put::builder()
            .table_name(table)
            .set_item(Some(event.to_item()))
            .build()
            .map_err(|error| format!("could not build the EVENT put: {error}"))?;
        writes.push(TransactWriteItem::builder().put(event_put).build());
    }

    client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
        .map_err(|error| format!("TransactWriteItems(TIMELINE+EVENT) failed: {error:?}"))?;

    Ok(())
}
