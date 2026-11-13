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
use aws_sdk_dynamodb::types::{AttributeValue, Delete, Put, TransactWriteItem, Update};
use data::item::DynamoItem;
use data::model::{Event, Report, ReportTargetType, Suggestion, Timeline};
use data::read;
use data::{Timestamp, keys};

use super::ratelimit::REPORT_ACTION;
use crate::transact;
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
        ReportTargetType::Event => Ok(read::get_event(client, table, timeline_id, target_id, true)
            .await
            .map_err(|error| error.to_string())?
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
    read::get_timeline(client, table, timeline_id, true)
        .await
        .map_err(|error| error.to_string())
}

pub async fn load_events(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Vec<Event>, StoreError> {
    read::list_events(client, table, timeline_id, true)
        .await
        .map_err(|error| error.to_string())
}

pub async fn load_suggestion(
    client: &Client,
    table: &str,
    timeline_id: &str,
    suggestion_id: &str,
) -> Result<Option<Suggestion>, StoreError> {
    read::get_suggestion(client, table, timeline_id, suggestion_id, true)
        .await
        .map_err(|error| error.to_string())
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
            .map_err(|error| operation_error("Query(SUGGESTION)", error))?;

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
        .map_err(|error| operation_error("GetItem(REPORT)", error))?;

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
        writes.push(
            transact::rate_limit_write(table, REPORT_ACTION, window)
                .map_err(ReportStoreError::Internal)?,
        );
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

fn transaction_error(
    error: &SdkError<TransactWriteItemsError>,
    kinds: &[WriteKind],
    direction: Direction,
) -> ReportStoreError {
    let failed = transact::conditional_failures(error);
    let mut retry_after = None;
    let mut target_failed = false;
    let mut report_failed = false;

    for &index in &failed {
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

    ReportStoreError::Internal(format!("TransactWriteItems(REPORT) failed: {error:?}"))
}

fn operation_error(operation: &str, error: impl std::fmt::Debug) -> StoreError {
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
