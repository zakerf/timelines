//! Vote persistence: one `TransactWriteItems` per vote.
//!
//! The transaction carries the rate-limit counters, the target's
//! `upvotes`/`downvotes` deltas, and the VOTE item. DynamoDB applies all of it
//! or none of it, so counters can never drift from the votes that produced
//! them (PRD §6 access pattern 7, PRD §8).

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{
    AttributeValue, CancellationReason, Delete, Put, TransactWriteItem, Update,
};
use data::item::DynamoItem;
use data::keys;
use data::model::{Event, Timeline, Vote, VoteDirection};

use super::ratelimit::{RateLimitWindow, VOTE_ACTION};

/// Counter movement for one vote change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VoteDelta {
    pub up: i64,
    pub down: i64,
}

/// The pure counter math: remove the old direction, then add the new one.
/// A withdrawal passes `None` as `new`, which reverses with no replacement.
pub fn vote_delta(old: Option<VoteDirection>, new: Option<VoteDirection>) -> VoteDelta {
    let old = old.map(VoteDirection::as_i64).unwrap_or(0);
    let new = new.map(VoteDirection::as_i64).unwrap_or(0);

    let mut delta = VoteDelta::default();
    if old == 1 {
        delta.up -= 1;
    } else if old == -1 {
        delta.down -= 1;
    }
    if new == 1 {
        delta.up += 1;
    } else if new == -1 {
        delta.down += 1;
    }
    delta
}

/// The target level for a vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteTarget {
    Timeline,
    Event(String),
}

/// What the transaction produced, for the HTMX fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteOutcome {
    pub upvotes: i64,
    pub downvotes: i64,
    pub vote: Option<VoteDirection>,
}

/// A refusal the transaction can produce after the handler has read the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteStoreError {
    /// One of the fixed-window counters is full.
    RateLimited {
        retry_after: i64,
    },
    /// The VOTE item changed between the read and the transaction.
    Conflict,
    /// The Timeline or Event disappeared between the read and the transaction.
    Missing,
    Internal(String),
}

/// One write plus the label used to interpret a cancellation reason.
#[derive(Debug, Clone, Copy)]
enum WriteKind {
    RateLimit { retry_after: i64 },
    Target,
    Vote,
}

pub async fn load_timeline(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Option<Timeline>, VoteStoreError> {
    let response = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .consistent_read(true)
        .send()
        .await
        .map_err(|error| internal("GetItem(TIMELINE)", error))?;

    match response.item {
        Some(item) => Timeline::from_item(item)
            .map(Some)
            .map_err(|error| VoteStoreError::Internal(format!("decode TIMELINE: {error}"))),
        None => Ok(None),
    }
}

/// Event rows carry their timestamp in the sort key, so an event id lookup is a
/// partition query followed by an id match. The page read already does the same
/// work; vote volume is tiny and the query is bounded by one timeline.
pub async fn load_event(
    client: &Client,
    table: &str,
    timeline_id: &str,
    event_id: &str,
) -> Result<Option<Event>, VoteStoreError> {
    let mut start_key = None;

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
            .consistent_read(true);

        if let Some(key) = start_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let response = request
            .send()
            .await
            .map_err(|error| internal("Query(EVENT)", error))?;

        for item in response.items.unwrap_or_default() {
            let event = Event::from_item(item)
                .map_err(|error| VoteStoreError::Internal(format!("decode EVENT: {error}")))?;
            if event.event_id == event_id {
                return Ok(Some(event));
            }
        }

        match response.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => return Ok(None),
        }
    }
}

pub async fn load_vote(
    client: &Client,
    table: &str,
    account_id: &str,
    timeline_id: &str,
    event_id: Option<&str>,
) -> Result<Option<Vote>, VoteStoreError> {
    let sk = match event_id {
        Some(event_id) => keys::event_vote_sk(timeline_id, event_id),
        None => keys::timeline_vote_sk(timeline_id),
    };

    let response = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::vote_pk(account_id)))
        .key("SK", AttributeValue::S(sk))
        .consistent_read(true)
        .send()
        .await
        .map_err(|error| internal("GetItem(VOTE)", error))?;

    match response.item {
        Some(item) => Vote::from_item(item)
            .map(Some)
            .map_err(|error| VoteStoreError::Internal(format!("decode VOTE: {error}"))),
        None => Ok(None),
    }
}

/// Own-vote state for a whole timeline: one Query on the account partition
/// (PRD §6 access pattern 3).
pub async fn list_votes(
    client: &Client,
    table: &str,
    account_id: &str,
    timeline_id: &str,
) -> Result<Vec<Vote>, VoteStoreError> {
    let mut start_key = None;
    let mut votes = Vec::new();

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(keys::vote_pk(account_id)))
            .expression_attribute_values(
                ":prefix",
                AttributeValue::S(keys::vote_timeline_prefix(timeline_id)),
            )
            .consistent_read(true);

        if let Some(key) = start_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let response = request
            .send()
            .await
            .map_err(|error| internal("Query(VOTE)", error))?;

        for item in response.items.unwrap_or_default() {
            let vote = Vote::from_item(item)
                .map_err(|error| VoteStoreError::Internal(format!("decode VOTE: {error}")))?;
            // The sort-key prefix has no trailing `#`, so keep only this
            // timeline's votes (a timeline id can prefix another id).
            if vote.timeline_id == timeline_id {
                votes.push(vote);
            }
        }

        match response.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => return Ok(votes),
        }
    }
}

/// Everything one vote transaction needs, grouped so the seam stays readable.
pub struct VoteCommit<'a> {
    pub account_id: &'a str,
    pub timeline: &'a Timeline,
    pub event: Option<&'a Event>,
    pub old_vote: Option<VoteDirection>,
    pub new_vote: Option<VoteDirection>,
    pub windows: &'a [RateLimitWindow],
    pub created_at: &'a data::Timestamp,
}

/// Cast, change, or withdraw a vote in one transaction.
pub async fn cast_vote(
    client: &Client,
    table: &str,
    commit: VoteCommit<'_>,
) -> Result<VoteOutcome, VoteStoreError> {
    let VoteCommit {
        account_id,
        timeline,
        event,
        old_vote,
        new_vote,
        windows,
        created_at,
    } = commit;

    let delta = vote_delta(old_vote, new_vote);
    let mut writes = Vec::with_capacity(windows.len() + 2);
    let mut kinds = Vec::with_capacity(windows.len() + 2);

    for window in windows {
        writes.push(rate_limit_write(table, window)?);
        kinds.push(WriteKind::RateLimit {
            retry_after: window.retry_after,
        });
    }

    writes.push(counter_write(table, timeline, event, delta)?);
    kinds.push(WriteKind::Target);

    if old_vote != new_vote {
        writes.push(vote_write(
            table, account_id, timeline, event, old_vote, new_vote, created_at,
        )?);
        kinds.push(WriteKind::Vote);
    }

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => {}
        Err(error) => return Err(transaction_error(&error, &kinds)),
    }

    let (upvotes, downvotes) = match event {
        Some(event) => (event.upvotes + delta.up, event.downvotes + delta.down),
        None => (timeline.upvotes + delta.up, timeline.downvotes + delta.down),
    };

    Ok(VoteOutcome {
        upvotes,
        downvotes,
        vote: new_vote,
    })
}

fn rate_limit_write(
    table: &str,
    window: &RateLimitWindow,
) -> Result<TransactWriteItem, VoteStoreError> {
    let update = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(window.partition.pk()))
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(VOTE_ACTION, &window.bucket)),
        )
        .update_expression("ADD #count :one SET #item_type = :item_type, #expires_at = :expires_at")
        .condition_expression("attribute_not_exists(#count) OR #count < :limit")
        .expression_attribute_names("#count", "count")
        .expression_attribute_names("#item_type", "itemType")
        .expression_attribute_names("#expires_at", "expiresAt")
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .expression_attribute_values(":limit", AttributeValue::N(window.limit.to_string()))
        .expression_attribute_values(":item_type", AttributeValue::S("RATELIMIT".to_owned()))
        .expression_attribute_values(
            ":expires_at",
            AttributeValue::N(window.expires_at.to_string()),
        )
        .build()
        .map_err(|error| VoteStoreError::Internal(format!("build RATELIMIT update: {error}")))?;

    Ok(TransactWriteItem::builder().update(update).build())
}

fn counter_write(
    table: &str,
    timeline: &Timeline,
    event: Option<&Event>,
    delta: VoteDelta,
) -> Result<TransactWriteItem, VoteStoreError> {
    let (pk, sk) = match event {
        Some(event) => (event.pk(), event.sk()),
        None => (timeline.pk(), timeline.sk()),
    };

    let update = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .update_expression("ADD #upvotes :up_delta, #downvotes :down_delta")
        .condition_expression(
            "attribute_exists(PK) AND #upvotes >= :need_up AND #downvotes >= :need_down",
        )
        .expression_attribute_names("#upvotes", "upvotes")
        .expression_attribute_names("#downvotes", "downvotes")
        .expression_attribute_values(":up_delta", AttributeValue::N(delta.up.to_string()))
        .expression_attribute_values(":down_delta", AttributeValue::N(delta.down.to_string()))
        .expression_attribute_values(
            ":need_up",
            AttributeValue::N((-delta.up).max(0).to_string()),
        )
        .expression_attribute_values(
            ":need_down",
            AttributeValue::N((-delta.down).max(0).to_string()),
        )
        .build()
        .map_err(|error| VoteStoreError::Internal(format!("build counter update: {error}")))?;

    Ok(TransactWriteItem::builder().update(update).build())
}

fn vote_write(
    table: &str,
    account_id: &str,
    timeline: &Timeline,
    event: Option<&Event>,
    old_vote: Option<VoteDirection>,
    new_vote: Option<VoteDirection>,
    created_at: &data::Timestamp,
) -> Result<TransactWriteItem, VoteStoreError> {
    let sk = match event {
        Some(event) => keys::event_vote_sk(&timeline.id, &event.event_id),
        None => keys::timeline_vote_sk(&timeline.id),
    };

    match (old_vote, new_vote) {
        (None, Some(direction)) => {
            let vote = Vote {
                account_id: account_id.to_owned(),
                timeline_id: timeline.id.clone(),
                event_id: event.map(|event| event.event_id.clone()),
                direction,
                created_at: created_at.clone(),
            };
            let put = Put::builder()
                .table_name(table)
                .set_item(Some(vote.to_item()))
                .condition_expression("attribute_not_exists(PK)")
                .build()
                .map_err(|error| VoteStoreError::Internal(format!("build VOTE put: {error}")))?;
            Ok(TransactWriteItem::builder().put(put).build())
        }
        (Some(old), Some(direction)) => {
            let vote = Vote {
                account_id: account_id.to_owned(),
                timeline_id: timeline.id.clone(),
                event_id: event.map(|event| event.event_id.clone()),
                direction,
                created_at: created_at.clone(),
            };
            let put = Put::builder()
                .table_name(table)
                .set_item(Some(vote.to_item()))
                .condition_expression("attribute_exists(PK) AND #direction = :old_direction")
                .expression_attribute_names("#direction", "direction")
                .expression_attribute_values(
                    ":old_direction",
                    AttributeValue::N(old.as_i64().to_string()),
                )
                .build()
                .map_err(|error| VoteStoreError::Internal(format!("build VOTE put: {error}")))?;
            Ok(TransactWriteItem::builder().put(put).build())
        }
        (Some(old), None) => {
            let delete = Delete::builder()
                .table_name(table)
                .key("PK", AttributeValue::S(keys::vote_pk(account_id)))
                .key("SK", AttributeValue::S(sk))
                .condition_expression("attribute_exists(PK) AND #direction = :old_direction")
                .expression_attribute_names("#direction", "direction")
                .expression_attribute_values(
                    ":old_direction",
                    AttributeValue::N(old.as_i64().to_string()),
                )
                .build()
                .map_err(|error| VoteStoreError::Internal(format!("build VOTE delete: {error}")))?;
            Ok(TransactWriteItem::builder().delete(delete).build())
        }
        (None, None) => unreachable!("a no-op withdrawal writes no vote item"),
    }
}

fn transaction_error(
    error: &SdkError<TransactWriteItemsError>,
    kinds: &[WriteKind],
) -> VoteStoreError {
    if let Some(service_error) = error.as_service_error()
        && let TransactWriteItemsError::TransactionCanceledException(canceled) = service_error
    {
        let reasons: &[CancellationReason] = canceled.cancellation_reasons();
        let mut retry_after = None;
        let mut target_failed = false;
        let mut vote_failed = false;

        for (index, reason) in reasons.iter().enumerate() {
            if reason.code() != Some("ConditionalCheckFailed") {
                continue;
            }
            match kinds.get(index) {
                Some(WriteKind::RateLimit {
                    retry_after: seconds,
                }) => {
                    retry_after = Some(retry_after.unwrap_or(0).max(*seconds));
                }
                Some(WriteKind::Vote) => vote_failed = true,
                Some(WriteKind::Target) => target_failed = true,
                None => {}
            }
        }

        // A full counter is the most useful refusal when several conditions
        // fail in the same transaction.
        if let Some(retry_after) = retry_after {
            return VoteStoreError::RateLimited { retry_after };
        }
        if target_failed {
            return VoteStoreError::Missing;
        }
        if vote_failed {
            return VoteStoreError::Conflict;
        }
    }

    VoteStoreError::Internal(format!("TransactWriteItems(VOTE) failed: {error:?}"))
}

fn internal(operation: &str, error: impl std::fmt::Debug) -> VoteStoreError {
    VoteStoreError::Internal(format!("{operation} failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casting_up_adds_one_upvote() {
        assert_eq!(
            vote_delta(None, Some(VoteDirection::Up)),
            VoteDelta { up: 1, down: 0 }
        );
    }

    #[test]
    fn casting_down_adds_one_downvote() {
        assert_eq!(
            vote_delta(None, Some(VoteDirection::Down)),
            VoteDelta { up: 0, down: 1 }
        );
    }

    #[test]
    fn changing_up_to_down_moves_both_counters() {
        assert_eq!(
            vote_delta(Some(VoteDirection::Up), Some(VoteDirection::Down)),
            VoteDelta { up: -1, down: 1 }
        );
    }

    #[test]
    fn changing_down_to_up_moves_both_counters_back() {
        assert_eq!(
            vote_delta(Some(VoteDirection::Down), Some(VoteDirection::Up)),
            VoteDelta { up: 1, down: -1 }
        );
    }

    #[test]
    fn withdrawing_up_only_removes_the_upvote() {
        assert_eq!(
            vote_delta(Some(VoteDirection::Up), None),
            VoteDelta { up: -1, down: 0 }
        );
    }

    #[test]
    fn withdrawing_down_only_removes_the_downvote() {
        assert_eq!(
            vote_delta(Some(VoteDirection::Down), None),
            VoteDelta { up: 0, down: -1 }
        );
    }

    #[test]
    fn resubmitting_the_same_direction_moves_nothing() {
        assert_eq!(
            vote_delta(Some(VoteDirection::Up), Some(VoteDirection::Up)),
            VoteDelta { up: 0, down: 0 }
        );
        assert_eq!(
            vote_delta(Some(VoteDirection::Down), Some(VoteDirection::Down)),
            VoteDelta { up: 0, down: 0 }
        );
    }

    #[test]
    fn withdrawing_with_no_vote_is_a_no_op() {
        assert_eq!(vote_delta(None, None), VoteDelta { up: 0, down: 0 });
    }
}
