//! REPORT persistence: one `TransactWriteItems` per report or withdrawal.
//!
//! The transaction carries the rate-limit counters, the target's
//! `reportCount`/`flagged` update, and the REPORT item itself. DynamoDB applies
//! all of it or none of it, so a refused report never consumes a counter and a
//! counter can never drift from the REPORT rows that produced it (PRD §8).
//!
//! Reports are anonymous to the owner: nothing about the reporter is copied
//! onto the target item. The only cross-account link is the REPORT item's
//! `SK = ACCOUNT#<accountId>`, which no owner-facing read exposes.

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{
    AttributeValue, CancellationReason, Delete, Put, TransactWriteItem, Update,
};
use data::item::DynamoItem;
use data::model::{Event, Report, ReportTargetType, Suggestion, Timeline};
use data::{Timestamp, keys};

use super::ratelimit::REPORT_ACTION;
use crate::vote::ratelimit::RateLimitWindow;

/// Three distinct reporting accounts flag the target (PRD §8). One report per
/// account per target makes `reportCount` and distinct accounts the same number.
pub const FLAG_THRESHOLD: i64 = 3;

/// The flag is a pure function of the current distinct-report count, so a
/// withdrawal that drops the count back below three clears it again. Nothing is
/// hidden or removed either way.
pub fn flagged_for(report_count: i64) -> bool {
    report_count >= FLAG_THRESHOLD
}

/// Store failure kept as a message: handlers log it and return a coarse 500.
pub type StoreError = String;

/// The key and current report state of a reportable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRow {
    pub pk: String,
    pub sk: String,
    pub report_count: i64,
    pub flagged: bool,
}

/// What a committed report or withdrawal leaves on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportState {
    pub report_count: i64,
    pub flagged: bool,
}

/// Everything one report transaction needs, grouped so the seam stays readable.
pub struct ReportCommit<'a> {
    pub target_type: ReportTargetType,
    pub target_id: &'a str,
    pub account_id: &'a str,
    pub target: &'a TargetRow,
    pub windows: &'a [RateLimitWindow],
    pub created_at: &'a Timestamp,
}

/// A refusal the transaction can produce after the handler has read the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportStoreError {
    /// One of the fixed-window counters is full.
    RateLimited {
        retry_after: i64,
    },
    /// Another request from this account already created the REPORT item.
    AlreadyReported,
    /// Another request from this account already deleted the REPORT item.
    AlreadyWithdrawn,
    /// The target changed between the read and the transaction.
    Conflict,
    Internal(String),
}

/// Read the target row for a report. `timeline_id` scopes EVENT and SUGGESTION
/// targets; TIMELINE targets use `target_id` directly.
pub async fn load_target(
    client: &Client,
    table: &str,
    target_type: ReportTargetType,
    timeline_id: &str,
    target_id: &str,
) -> Result<Option<TargetRow>, StoreError> {
    match target_type {
        ReportTargetType::Timeline => {
            Ok(load_timeline(client, table, target_id)
                .await?
                .map(|timeline| TargetRow {
                    pk: timeline.pk(),
                    sk: timeline.sk(),
                    report_count: timeline.report_count,
                    flagged: timeline.flagged,
                }))
        }
        ReportTargetType::Event => Ok(load_events(client, table, timeline_id)
            .await?
            .into_iter()
            .find(|event| event.event_id == target_id)
            .map(|event| TargetRow {
                pk: event.pk(),
                sk: event.sk(),
                report_count: event.report_count,
                flagged: event.flagged,
            })),
        ReportTargetType::Suggestion => Ok(load_suggestion(client, table, timeline_id, target_id)
            .await?
            .map(|suggestion| TargetRow {
                pk: suggestion.pk(),
                sk: suggestion.sk(),
                report_count: suggestion.report_count,
                flagged: suggestion.flagged,
            })),
    }
}

pub async fn load_timeline(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Option<Timeline>, StoreError> {
    let response = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .consistent_read(true)
        .send()
        .await
        .map_err(|error| internal("GetItem(TIMELINE)", error))?;

    response
        .item
        .map(|item| Timeline::from_item(item).map_err(|error| format!("decode TIMELINE: {error}")))
        .transpose()
}

pub async fn load_events(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Vec<Event>, StoreError> {
    let mut events = Vec::new();
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
            events.push(Event::from_item(item).map_err(|error| format!("decode EVENT: {error}"))?);
        }

        match response.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => return Ok(events),
        }
    }
}

pub async fn load_suggestion(
    client: &Client,
    table: &str,
    timeline_id: &str,
    suggestion_id: &str,
) -> Result<Option<Suggestion>, StoreError> {
    let response = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::suggestion_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::suggestion_sk(suggestion_id)))
        .consistent_read(true)
        .send()
        .await
        .map_err(|error| internal("GetItem(SUGGESTION)", error))?;

    response
        .item
        .map(|item| {
            Suggestion::from_item(item).map_err(|error| format!("decode SUGGESTION: {error}"))
        })
        .transpose()
}

/// Every Suggestion in a Timeline partition, for the owner's report summary.
pub async fn load_suggestions(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Vec<Suggestion>, StoreError> {
    let mut suggestions = Vec::new();
    let mut start_key = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(keys::suggestion_pk(timeline_id)))
            .expression_attribute_values(
                ":prefix",
                AttributeValue::S(keys::SUGGESTION_SK_PREFIX.to_owned()),
            )
            .consistent_read(true);

        if let Some(key) = start_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let response = request
            .send()
            .await
            .map_err(|error| internal("Query(SUGGESTION)", error))?;

        for item in response.items.unwrap_or_default() {
            suggestions.push(
                Suggestion::from_item(item)
                    .map_err(|error| format!("decode SUGGESTION: {error}"))?,
            );
        }

        match response.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => return Ok(suggestions),
        }
    }
}

/// Does this account already hold a report on the target? Existence is the
/// idempotency key (PRD §8).
pub async fn has_report(
    client: &Client,
    table: &str,
    target_type: ReportTargetType,
    target_id: &str,
    account_id: &str,
) -> Result<bool, StoreError> {
    let response = client
        .get_item()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::report_pk(target_type, target_id)),
        )
        .key("SK", AttributeValue::S(keys::report_sk(account_id)))
        .consistent_read(true)
        .send()
        .await
        .map_err(|error| internal("GetItem(REPORT)", error))?;

    Ok(response.item.is_some())
}

/// Create one report in a single transaction with its counters.
pub async fn create_report(
    client: &Client,
    table: &str,
    commit: ReportCommit<'_>,
) -> Result<ReportState, ReportStoreError> {
    let ReportCommit {
        target_type,
        target_id,
        account_id,
        target,
        windows,
        created_at,
    } = commit;

    let report = Report {
        target_type,
        target_id: target_id.to_owned(),
        account_id: account_id.to_owned(),
        created_at: created_at.clone(),
    };

    let next_count = target.report_count + 1;
    let flagged = flagged_for(next_count);

    let mut writes = Vec::with_capacity(windows.len() + 2);
    let mut kinds = Vec::with_capacity(windows.len() + 2);

    for window in windows {
        writes.push(rate_limit_write(table, window)?);
        kinds.push(WriteKind::RateLimit {
            retry_after: window.retry_after,
        });
    }

    writes.push(target_write(table, target, 1, flagged)?);
    kinds.push(WriteKind::Target);

    let put = Put::builder()
        .table_name(table)
        .set_item(Some(report.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .build()
        .map_err(|error| ReportStoreError::Internal(format!("build REPORT put: {error}")))?;
    writes.push(TransactWriteItem::builder().put(put).build());
    kinds.push(WriteKind::Report);

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => Ok(ReportState {
            report_count: next_count,
            flagged,
        }),
        Err(error) => Err(transaction_error(&error, &kinds, Direction::Report)),
    }
}

/// Delete the caller's own report and reverse its counter in one transaction.
pub async fn withdraw_report(
    client: &Client,
    table: &str,
    target_type: ReportTargetType,
    target_id: &str,
    account_id: &str,
    target: &TargetRow,
) -> Result<ReportState, ReportStoreError> {
    let next_count = (target.report_count - 1).max(0);
    let flagged = flagged_for(next_count);

    let delete = Delete::builder()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::report_pk(target_type, target_id)),
        )
        .key("SK", AttributeValue::S(keys::report_sk(account_id)))
        .condition_expression("attribute_exists(PK)")
        .build()
        .map_err(|error| ReportStoreError::Internal(format!("build REPORT delete: {error}")))?;

    let writes = vec![
        target_write(table, target, -1, flagged)?,
        TransactWriteItem::builder().delete(delete).build(),
    ];
    let kinds = [WriteKind::Target, WriteKind::Report];

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => Ok(ReportState {
            report_count: next_count,
            flagged,
        }),
        Err(error) => Err(transaction_error(&error, &kinds, Direction::Withdraw)),
    }
}

#[derive(Debug, Clone, Copy)]
enum WriteKind {
    RateLimit { retry_after: i64 },
    Target,
    Report,
}

/// Which refusal a REPORT-item condition failure means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Report,
    Withdraw,
}

fn target_write(
    table: &str,
    target: &TargetRow,
    delta: i64,
    flagged: bool,
) -> Result<TransactWriteItem, ReportStoreError> {
    let condition = if delta < 0 {
        "attribute_exists(PK) AND attribute_exists(#count) AND #count = :expected AND #count > :zero"
    } else {
        "attribute_exists(PK) AND (attribute_not_exists(#count) OR #count = :expected)"
    };

    let mut builder = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(target.pk.clone()))
        .key("SK", AttributeValue::S(target.sk.clone()))
        .update_expression("SET #flagged = :flagged ADD #count :delta")
        .condition_expression(condition)
        .expression_attribute_names("#count", "reportCount")
        .expression_attribute_names("#flagged", "flagged")
        .expression_attribute_values(":delta", AttributeValue::N(delta.to_string()))
        .expression_attribute_values(":flagged", AttributeValue::Bool(flagged))
        .expression_attribute_values(
            ":expected",
            AttributeValue::N(target.report_count.to_string()),
        );

    if delta < 0 {
        builder = builder.expression_attribute_values(":zero", AttributeValue::N("0".to_owned()));
    }

    let update = builder
        .build()
        .map_err(|error| ReportStoreError::Internal(format!("build target update: {error}")))?;

    Ok(TransactWriteItem::builder().update(update).build())
}

fn rate_limit_write(
    table: &str,
    window: &RateLimitWindow,
) -> Result<TransactWriteItem, ReportStoreError> {
    let update = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(window.partition.pk()))
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(REPORT_ACTION, &window.bucket)),
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
        .map_err(|error| ReportStoreError::Internal(format!("build RATELIMIT update: {error}")))?;

    Ok(TransactWriteItem::builder().update(update).build())
}

fn transaction_error(
    error: &SdkError<TransactWriteItemsError>,
    kinds: &[WriteKind],
    direction: Direction,
) -> ReportStoreError {
    if let Some(service_error) = error.as_service_error()
        && let TransactWriteItemsError::TransactionCanceledException(canceled) = service_error
    {
        let reasons: &[CancellationReason] = canceled.cancellation_reasons();
        let mut retry_after = None;
        let mut target_failed = false;
        let mut report_failed = false;

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
                Some(WriteKind::Target) => target_failed = true,
                Some(WriteKind::Report) => report_failed = true,
                None => {}
            }
        }

        // A full counter is the most useful refusal when several conditions
        // fail in the same transaction.
        if let Some(retry_after) = retry_after {
            return ReportStoreError::RateLimited { retry_after };
        }
        if target_failed {
            return ReportStoreError::Conflict;
        }
        if report_failed {
            return match direction {
                Direction::Report => ReportStoreError::AlreadyReported,
                Direction::Withdraw => ReportStoreError::AlreadyWithdrawn,
            };
        }
    }

    ReportStoreError::Internal(format!("TransactWriteItems(REPORT) failed: {error:?}"))
}

fn internal(operation: &str, error: impl std::fmt::Debug) -> StoreError {
    format!("{operation} failed: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_turns_on_at_three_and_follows_the_count_back_down() {
        assert!(!flagged_for(0));
        assert!(!flagged_for(2));
        assert!(flagged_for(3));
        assert!(flagged_for(4));
        assert!(
            !flagged_for(2),
            "a withdrawal below the threshold clears it"
        );
    }
}
