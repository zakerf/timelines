//! DynamoDB persistence for the first mutation: create a Timeline and its
//! initial Events in one `TransactWriteItems`.
//!
//! The Timeline `Put` is conditioned on `attribute_not_exists(PK)` so a
//! generated id can never overwrite an existing Timeline. `eventCount` is
//! stamped on the Timeline row in the same transaction as the Event rows, so a
//! publish never leaves a half-written timeline (PRD §6, access pattern 8).
//!
//! The PRD §8 publish limits ride the same transaction: two fixed-window
//! counters are incremented with `ADD` under a `< limit` condition, so a
//! refused publish writes nothing and never increments a counter.

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, Put, TransactWriteItem, Update};
use chrono::DateTime;
use data::item::{DynamoItem, ItemType};
use data::model::{Event, Timeline};
use data::{Timestamp, keys};

/// Per-Account publish limits (PRD §8).
pub const TIMELINES_PER_DAY: i64 = 10;
pub const EVENTS_PER_DAY: i64 = 100;
/// Per-hashed-IP Timeline publishes per day (PRD §8). Applies to the
/// `publish-timeline` action under the `RL#IP#<hash>` partition.
pub const IP_TIMELINES_PER_DAY: i64 = 30;

/// Fixed-window counter actions under `RL#ACCOUNT#<id>` (and, for the Timeline
/// limit, `RL#IP#<hash>`).
pub const TIMELINE_LIMIT_ACTION: &str = "publish-timeline";
pub const EVENT_LIMIT_ACTION: &str = "publish-event";

/// Keep a window's counter one day past its bucket so the TTL never races a
/// still-current window.
const LIMIT_TTL_SECONDS: i64 = 2 * 86_400;

/// Store failure: a limit refusal is a `429`; everything else is a coarse 500.
#[derive(Debug)]
pub enum StoreError {
    RateLimited { retry_after_seconds: u64 },
    Other(String),
}

pub async fn create_timeline(
    client: &Client,
    table: &str,
    timeline: &Timeline,
    events: &[Event],
    now: &Timestamp,
    ip_hash: Option<&str>,
) -> Result<(), StoreError> {
    let event_count = i64::try_from(events.len()).expect("an event list fits in i64");
    if event_count > EVENTS_PER_DAY {
        return Err(StoreError::RateLimited {
            retry_after_seconds: seconds_until_day_end(now),
        });
    }

    let bucket = now.date_bucket();
    let mut writes = Vec::with_capacity(events.len() + 4);

    // The limit counters come first so a cancellation reason's index maps
    // straight back to the limit that refused the write.
    writes.push(rate_limit_write(
        table,
        &keys::ratelimit_account_pk(&timeline.owner_id),
        TIMELINE_LIMIT_ACTION,
        &bucket,
        1,
        TIMELINES_PER_DAY - 1,
    )?);
    writes.push(rate_limit_write(
        table,
        &keys::ratelimit_account_pk(&timeline.owner_id),
        EVENT_LIMIT_ACTION,
        &bucket,
        event_count,
        EVENTS_PER_DAY - event_count,
    )?);
    if let Some(ip_hash) = ip_hash {
        writes.push(rate_limit_write(
            table,
            &keys::ratelimit_ip_pk(ip_hash),
            TIMELINE_LIMIT_ACTION,
            &bucket,
            1,
            IP_TIMELINES_PER_DAY - 1,
        )?);
    }
    let counter_count = writes.len();

    let timeline_put = Put::builder()
        .table_name(table)
        .set_item(Some(timeline.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .build()
        .map_err(|error| StoreError::Other(format!("could not build the TIMELINE put: {error}")))?;
    writes.push(TransactWriteItem::builder().put(timeline_put).build());

    for event in events {
        let event_put = Put::builder()
            .table_name(table)
            .set_item(Some(event.to_item()))
            .build()
            .map_err(|error| {
                StoreError::Other(format!("could not build the EVENT put: {error}"))
            })?;
        writes.push(TransactWriteItem::builder().put(event_put).build());
    }

    let error = match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => return Ok(()),
        Err(error) => error,
    };

    if let Some(TransactWriteItemsError::TransactionCanceledException(canceled)) =
        error.as_service_error()
    {
        // Exactly the leading counters precede the content writes.
        let limit_refused = canceled
            .cancellation_reasons()
            .iter()
            .take(counter_count)
            .any(|reason| reason.code() == Some("ConditionalCheckFailed"));
        if limit_refused {
            return Err(StoreError::RateLimited {
                retry_after_seconds: seconds_until_day_end(now),
            });
        }
    }

    Err(StoreError::Other(format!(
        "TransactWriteItems(TIMELINE+EVENT) failed: {error:?}"
    )))
}

/// Append one Event directly to an existing Timeline (PRD §5, §6 access pattern
/// 8). One transaction: the account's daily Event counter, the EVENT row, and
/// the `eventCount`/`updatedAt` bump commit together or not at all.
pub async fn append_event(
    client: &Client,
    table: &str,
    timeline_id: &str,
    owner_id: &str,
    event: &Event,
    now: &Timestamp,
) -> Result<(), StoreError> {
    let bucket = now.date_bucket();

    let mut writes = vec![rate_limit_write(
        table,
        &keys::ratelimit_account_pk(owner_id),
        EVENT_LIMIT_ACTION,
        &bucket,
        1,
        EVENTS_PER_DAY - 1,
    )?];

    let event_put = Put::builder()
        .table_name(table)
        .set_item(Some(event.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .build()
        .map_err(|error| StoreError::Other(format!("could not build the EVENT put: {error}")))?;
    writes.push(TransactWriteItem::builder().put(event_put).build());

    let timeline_bump = Update::builder()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::timeline_pk(timeline_id)),
        )
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .update_expression("SET #updatedAt = :now ADD #eventCount :one")
        .condition_expression("attribute_exists(PK)")
        .expression_attribute_names("#updatedAt", "updatedAt")
        .expression_attribute_names("#eventCount", "eventCount")
        .expression_attribute_values(":now", AttributeValue::S(now.as_str().to_owned()))
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .build()
        .map_err(|error| StoreError::Other(format!("could not build the TIMELINE bump: {error}")))?;
    writes.push(TransactWriteItem::builder().update(timeline_bump).build());

    let error = match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => return Ok(()),
        Err(error) => error,
    };

    if let Some(TransactWriteItemsError::TransactionCanceledException(canceled)) =
        error.as_service_error()
    {
        // The single counter leads the write list.
        let limit_refused = canceled
            .cancellation_reasons()
            .first()
            .is_some_and(|reason| reason.code() == Some("ConditionalCheckFailed"));
        if limit_refused {
            return Err(StoreError::RateLimited {
                retry_after_seconds: seconds_until_day_end(now),
            });
        }
    }

    Err(StoreError::Other(format!(
        "TransactWriteItems(APPEND EVENT) failed: {error:?}"
    )))
}

fn rate_limit_write(
    table: &str,
    pk: &str,
    action: &str,
    window_bucket: &str,
    increment: i64,
    max_before: i64,
) -> Result<TransactWriteItem, StoreError> {
    let update = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(pk.to_owned()))
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(action, window_bucket)),
        )
        .update_expression(
            "SET #itemType = :item_type, #expiresAt = if_not_exists(#expiresAt, :expires_at) \
             ADD #count :increment",
        )
        .condition_expression("attribute_not_exists(#count) OR #count <= :max_before")
        .expression_attribute_names("#itemType", "itemType")
        .expression_attribute_names("#expiresAt", "expiresAt")
        .expression_attribute_names("#count", "count")
        .expression_attribute_values(
            ":item_type",
            AttributeValue::S(ItemType::RateLimit.as_str().to_owned()),
        )
        .expression_attribute_values(
            ":expires_at",
            AttributeValue::N(
                (data::timestamp::now_epoch_seconds() + LIMIT_TTL_SECONDS).to_string(),
            ),
        )
        .expression_attribute_values(":increment", AttributeValue::N(increment.to_string()))
        .expression_attribute_values(":max_before", AttributeValue::N(max_before.to_string()))
        .build()
        .map_err(|error| {
            StoreError::Other(format!(
                "could not build the {action} rate-limit update: {error}"
            ))
        })?;

    Ok(TransactWriteItem::builder().update(update).build())
}

/// Seconds left in the current UTC day, for `Retry-After`.
fn seconds_until_day_end(now: &Timestamp) -> u64 {
    let parsed = DateTime::parse_from_rfc3339(now.as_str())
        .expect("canonical timestamps always parse as RFC 3339");
    let elapsed = u64::try_from(parsed.timestamp().rem_euclid(86_400))
        .expect("seconds since UTC midnight fit in u64");
    (86_400 - elapsed).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_the_seconds_left_in_the_utc_day() {
        let late = Timestamp::parse("2026-11-08T23:59:00Z").expect("timestamp");
        assert_eq!(seconds_until_day_end(&late), 60);

        let midnight = Timestamp::parse("2026-11-08T00:00:00Z").expect("timestamp");
        assert_eq!(seconds_until_day_end(&midnight), 86_400);
    }
}
