//! DynamoDB persistence for Suggestions (PRD §6 access patterns 5/6/9/10).
//!
//! Submission and review are each one `TransactWriteItems`: the state flip and
//! every side effect (the rate-limit counters, the pending cap, the EVENT row,
//! `eventCount`) commit together or not at all.

use std::collections::HashMap;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, Delete, Put, TransactWriteItem};
use data::item::DynamoItem;
use data::model::{Event, Suggestion, Timeline};
use data::{Timestamp, keys};

/// Store failure kept as a message: handlers log it and return a coarse 500.
pub type StoreError = String;

/// PRD §8 limits for submitting a Suggestion. All tunable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitLimits {
    pub account_hourly: i64,
    pub account_daily: i64,
    pub ip_hourly: i64,
    pub pending_cap: i64,
}

impl Default for SubmitLimits {
    fn default() -> Self {
        Self {
            account_hourly: 5,
            account_daily: 20,
            ip_hourly: 20,
            pending_cap: 10,
        }
    }
}

/// Which counter refused a submission, for `Retry-After` and the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitLimit {
    PendingCap,
    AccountHourly,
    AccountDaily,
    IpHourly,
}

impl SubmitLimit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingCap => "pending_cap",
            Self::AccountHourly => "account_hourly",
            Self::AccountDaily => "account_daily",
            Self::IpHourly => "ip_hourly",
        }
    }
}

#[derive(Debug)]
pub enum SubmitFailure {
    RateLimited(SubmitLimit),
    Store(StoreError),
}

/// Submit a Suggestion: the row itself plus every guard, atomically.
///
/// Item order is part of the cancellation-reason mapping: 0 SUGGESTION,
/// 1 SUGGESTCOUNT, 2 account/hour, 3 account/day, 4 IP/hour.
pub async fn submit(
    client: &Client,
    table: &str,
    suggestion: &Suggestion,
    limits: &SubmitLimits,
    now: &Timestamp,
    ip_hash: Option<&str>,
) -> Result<(), SubmitFailure> {
    let hour = now.hour_bucket();
    let day = now.date_bucket();
    let ttl_hour = data::timestamp::now_epoch_seconds() + 2 * 60 * 60;
    let ttl_day = data::timestamp::now_epoch_seconds() + 26 * 60 * 60;

    let suggestion_put = Put::builder()
        .table_name(table)
        .set_item(Some(suggestion.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .build()
        .map_err(|error| SubmitFailure::Store(format!("build SUGGESTION put: {error}")))?;

    let mut writes = vec![
        TransactWriteItem::builder().put(suggestion_put).build(),
        TransactWriteItem::builder()
            .update(suggestcount_update(table, suggestion, limits.pending_cap)?)
            .build(),
        TransactWriteItem::builder()
            .update(rate_limit_update(
                table,
                &keys::ratelimit_account_pk(&suggestion.submitted_by),
                "suggest",
                &hour,
                limits.account_hourly,
                ttl_hour,
            )?)
            .build(),
        TransactWriteItem::builder()
            .update(rate_limit_update(
                table,
                &keys::ratelimit_account_pk(&suggestion.submitted_by),
                "suggest",
                &day,
                limits.account_daily,
                ttl_day,
            )?)
            .build(),
    ];

    if let Some(hash) = ip_hash {
        writes.push(
            TransactWriteItem::builder()
                .update(rate_limit_update(
                    table,
                    &keys::ratelimit_ip_pk(hash),
                    "suggest",
                    &hour,
                    limits.ip_hourly,
                    ttl_hour,
                )?)
                .build(),
        );
    }

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => match first_conditional_failure(&error) {
            Some(1) => Err(SubmitFailure::RateLimited(SubmitLimit::PendingCap)),
            Some(2) => Err(SubmitFailure::RateLimited(SubmitLimit::AccountHourly)),
            Some(3) => Err(SubmitFailure::RateLimited(SubmitLimit::AccountDaily)),
            Some(4) => Err(SubmitFailure::RateLimited(SubmitLimit::IpHourly)),
            Some(0) => Err(SubmitFailure::Store(
                "generated suggestion id collided".to_owned(),
            )),
            _ => Err(SubmitFailure::Store(format!(
                "transact write failed: {error:?}"
            ))),
        },
    }
}

/// The owner's review stamp.
#[derive(Debug, Clone, Copy)]
pub struct Review<'a> {
    pub reviewer: &'a str,
    pub note: Option<&'a str>,
    pub now: &'a Timestamp,
}

/// An accepted EDIT before and after it is applied.
#[derive(Debug, Clone)]
pub struct EditChange {
    pub original: Event,
    pub updated: Event,
}

#[derive(Debug)]
pub enum ReviewOutcome<'a> {
    ApproveAppend(&'a Event),
    ApproveEdit(&'a EditChange),
    Reject,
}

#[derive(Debug)]
pub enum ReviewFailure {
    NotPending,
    Conflict(&'static str),
    Store(StoreError),
}

/// Approve or reject a pending Suggestion in one transaction.
///
/// Item order: 0 SUGGESTION, 1 SUGGESTCOUNT, then the content writes.
pub async fn review(
    client: &Client,
    table: &str,
    suggestion: &Suggestion,
    review: Review<'_>,
    outcome: ReviewOutcome<'_>,
) -> Result<(), ReviewFailure> {
    let reviewed = match outcome {
        ReviewOutcome::Reject => "REJECTED",
        ReviewOutcome::ApproveAppend(_) | ReviewOutcome::ApproveEdit(_) => "APPROVED",
    };

    let state_update = suggestion_review_update(table, suggestion, review, reviewed)?;
    let counter_update = suggestcount_decrement(table, suggestion)?;

    let mut writes = vec![
        TransactWriteItem::builder().update(state_update).build(),
        TransactWriteItem::builder().update(counter_update).build(),
    ];

    match outcome {
        ReviewOutcome::Reject => {}
        ReviewOutcome::ApproveAppend(event) => {
            writes.push(event_put(table, event, "attribute_not_exists(PK)")?);
            writes.push(
                TransactWriteItem::builder()
                    .update(timeline_bump(table, suggestion, review.now)?)
                    .build(),
            );
        }
        ReviewOutcome::ApproveEdit(change) => {
            if change.original.sk() == change.updated.sk() {
                writes.push(event_put(table, &change.updated, "attribute_exists(PK)")?);
            } else {
                let delete = Delete::builder()
                    .table_name(table)
                    .key("PK", AttributeValue::S(change.original.pk()))
                    .key("SK", AttributeValue::S(change.original.sk()))
                    .condition_expression("attribute_exists(PK)")
                    .build()
                    .map_err(|error| {
                        ReviewFailure::Store(format!("build EVENT delete: {error}"))
                    })?;
                writes.push(TransactWriteItem::builder().delete(delete).build());
                writes.push(event_put(
                    table,
                    &change.updated,
                    "attribute_not_exists(PK)",
                )?);
            }
        }
    }

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => match first_conditional_failure(&error) {
            Some(0) => Err(ReviewFailure::NotPending),
            Some(1) => Err(ReviewFailure::Store(
                "the pending suggestion counter is missing".to_owned(),
            )),
            Some(_) => Err(ReviewFailure::Conflict(
                "the event changed before the review committed",
            )),
            None => Err(ReviewFailure::Store(format!(
                "transact write failed: {error:?}"
            ))),
        },
    }
}

pub async fn get_timeline(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Option<Timeline>, StoreError> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await
        .map_err(|error| format!("GetItem(TIMELINE) failed: {error:?}"))?;

    output
        .item()
        .map(|item| Timeline::from_item(item.clone()).map_err(|error| error.to_string()))
        .transpose()
}

pub async fn get_suggestion(
    client: &Client,
    table: &str,
    timeline_id: &str,
    suggestion_id: &str,
) -> Result<Option<Suggestion>, StoreError> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::suggestion_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::suggestion_sk(suggestion_id)))
        .send()
        .await
        .map_err(|error| format!("GetItem(SUGGESTION) failed: {error:?}"))?;

    output
        .item()
        .map(|item| Suggestion::from_item(item.clone()).map_err(|error| error.to_string()))
        .transpose()
}

/// Access pattern 6: the public pending list in one timeline partition.
pub async fn list_pending(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> Result<Vec<Suggestion>, StoreError> {
    let mut suggestions = Vec::new();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(keys::suggestion_pk(timeline_id)))
            .expression_attribute_values(
                ":prefix",
                AttributeValue::S(keys::SUGGESTION_SK_PREFIX.to_owned()),
            );
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request
            .send()
            .await
            .map_err(|error| format!("Query(SUGGESTION) failed: {error:?}"))?;

        for item in output.items() {
            suggestions.push(Suggestion::from_item(item.clone()).map_err(|e| e.to_string())?);
        }
        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    suggestions.retain(|suggestion| suggestion.state == data::model::SuggestionState::Pending);
    suggestions.sort_by(|left, right| right.submitted_at.cmp(&left.submitted_at));
    Ok(suggestions)
}

/// Access pattern 5: the owner queue across every owned Timeline, newest first.
pub async fn owner_queue(
    client: &Client,
    table: &str,
    owner_id: &str,
) -> Result<Vec<Suggestion>, StoreError> {
    let mut suggestions = Vec::new();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .index_name("GSI1")
            .key_condition_expression("GSI1PK = :pk")
            .expression_attribute_values(
                ":pk",
                AttributeValue::S(keys::suggestion_gsi1_pk(owner_id)),
            )
            .scan_index_forward(false);
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request
            .send()
            .await
            .map_err(|error| format!("Query(GSI1) failed: {error:?}"))?;

        for item in output.items() {
            suggestions.push(Suggestion::from_item(item.clone()).map_err(|e| e.to_string())?);
        }
        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    Ok(suggestions)
}

fn suggestcount_update(
    table: &str,
    suggestion: &Suggestion,
    pending_cap: i64,
) -> Result<aws_sdk_dynamodb::types::Update, SubmitFailure> {
    aws_sdk_dynamodb::types::Update::builder()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::suggestcount_pk(&suggestion.timeline_id)),
        )
        .key(
            "SK",
            AttributeValue::S(keys::suggestcount_sk(&suggestion.submitted_by)),
        )
        .update_expression("SET #itemType = if_not_exists(#itemType, :itemType) ADD #pending :one")
        .condition_expression("attribute_not_exists(#pending) OR #pending < :cap")
        .expression_attribute_names("#itemType", "itemType")
        .expression_attribute_names("#pending", "pending")
        .expression_attribute_values(":itemType", AttributeValue::S("SUGGESTCOUNT".to_owned()))
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .expression_attribute_values(":cap", AttributeValue::N(pending_cap.to_string()))
        .build()
        .map_err(|error| SubmitFailure::Store(format!("build SUGGESTCOUNT update: {error}")))
}

fn suggestcount_decrement(
    table: &str,
    suggestion: &Suggestion,
) -> Result<aws_sdk_dynamodb::types::Update, ReviewFailure> {
    aws_sdk_dynamodb::types::Update::builder()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::suggestcount_pk(&suggestion.timeline_id)),
        )
        .key(
            "SK",
            AttributeValue::S(keys::suggestcount_sk(&suggestion.submitted_by)),
        )
        .update_expression("ADD #pending :neg")
        .condition_expression("attribute_exists(#pending) AND #pending > :zero")
        .expression_attribute_names("#pending", "pending")
        .expression_attribute_values(":neg", AttributeValue::N("-1".to_owned()))
        .expression_attribute_values(":zero", AttributeValue::N("0".to_owned()))
        .build()
        .map_err(|error| ReviewFailure::Store(format!("build SUGGESTCOUNT update: {error}")))
}

fn rate_limit_update(
    table: &str,
    pk: &str,
    action: &str,
    bucket: &str,
    limit: i64,
    expires_at: i64,
) -> Result<aws_sdk_dynamodb::types::Update, SubmitFailure> {
    aws_sdk_dynamodb::types::Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(pk.to_owned()))
        .key("SK", AttributeValue::S(keys::ratelimit_sk(action, bucket)))
        .update_expression(
            "SET #itemType = if_not_exists(#itemType, :itemType), #expiresAt = :expiresAt ADD #count :one",
        )
        .condition_expression("attribute_not_exists(#count) OR #count < :limit")
        .expression_attribute_names("#itemType", "itemType")
        .expression_attribute_names("#expiresAt", "expiresAt")
        .expression_attribute_names("#count", "count")
        .expression_attribute_values(":itemType", AttributeValue::S("RATELIMIT".to_owned()))
        .expression_attribute_values(":expiresAt", AttributeValue::N(expires_at.to_string()))
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .expression_attribute_values(":limit", AttributeValue::N(limit.to_string()))
        .build()
        .map_err(|error| SubmitFailure::Store(format!("build RATELIMIT update: {error}")))
}

fn suggestion_review_update(
    table: &str,
    suggestion: &Suggestion,
    review: Review<'_>,
    state: &str,
) -> Result<aws_sdk_dynamodb::types::Update, ReviewFailure> {
    let mut expression =
        String::from("SET #state = :state, reviewedBy = :reviewedBy, reviewedAt = :reviewedAt");
    let mut builder = aws_sdk_dynamodb::types::Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(suggestion.pk()))
        .key("SK", AttributeValue::S(suggestion.sk()))
        .condition_expression("attribute_exists(PK) AND #state = :pending")
        .expression_attribute_names("#state", "state")
        .expression_attribute_values(":state", AttributeValue::S(state.to_owned()))
        .expression_attribute_values(
            ":pending",
            AttributeValue::S(data::model::SuggestionState::Pending.as_str().to_owned()),
        )
        .expression_attribute_values(":reviewedBy", AttributeValue::S(review.reviewer.to_owned()))
        .expression_attribute_values(
            ":reviewedAt",
            AttributeValue::S(review.now.as_str().to_owned()),
        );

    if let Some(note) = review.note {
        expression.push_str(", reviewNote = :reviewNote");
        builder =
            builder.expression_attribute_values(":reviewNote", AttributeValue::S(note.to_owned()));
    }
    expression.push_str(" REMOVE GSI1PK, GSI1SK");

    builder
        .update_expression(expression)
        .build()
        .map_err(|error| ReviewFailure::Store(format!("build SUGGESTION update: {error}")))
}

fn event_put(
    table: &str,
    event: &Event,
    condition: &str,
) -> Result<TransactWriteItem, ReviewFailure> {
    let put = Put::builder()
        .table_name(table)
        .set_item(Some(event.to_item()))
        .condition_expression(condition)
        .build()
        .map_err(|error| ReviewFailure::Store(format!("build EVENT put: {error}")))?;
    Ok(TransactWriteItem::builder().put(put).build())
}

fn timeline_bump(
    table: &str,
    suggestion: &Suggestion,
    now: &Timestamp,
) -> Result<aws_sdk_dynamodb::types::Update, ReviewFailure> {
    aws_sdk_dynamodb::types::Update::builder()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::timeline_pk(&suggestion.timeline_id)),
        )
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .update_expression("SET #updatedAt = :now ADD #eventCount :one")
        .condition_expression("attribute_exists(PK)")
        .expression_attribute_names("#updatedAt", "updatedAt")
        .expression_attribute_names("#eventCount", "eventCount")
        .expression_attribute_values(":now", AttributeValue::S(now.as_str().to_owned()))
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .build()
        .map_err(|error| ReviewFailure::Store(format!("build TIMELINE update: {error}")))
}

/// The index of the first item whose condition failed in a canceled
/// transaction, if it was a conditional cancellation at all.
fn first_conditional_failure(error: &SdkError<TransactWriteItemsError>) -> Option<usize> {
    let SdkError::ServiceError(service) = error else {
        return None;
    };
    let TransactWriteItemsError::TransactionCanceledException(canceled) = service.err() else {
        return None;
    };
    canceled
        .cancellation_reasons()
        .iter()
        .position(|reason| reason.code() == Some("ConditionalCheckFailed"))
}
