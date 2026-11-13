//! Shared `TransactWriteItems` building blocks.
//!
//! Every write transaction counts its rate-limit windows first and its item
//! last. Only the mapping from a failed condition back to a domain refusal
//! differs per store, so that mapping stays with each store while the counter
//! write and the cancellation decoding live here.

use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::error::TransactionCanceledException;
use aws_sdk_dynamodb::types::{AttributeValue, TransactWriteItem, Update};
use data::keys;

use crate::vote::ratelimit::RateLimitWindow;

/// One fixed-window counter write: increment unless the window is full.
pub fn rate_limit_write(
    table: &str,
    action: &str,
    window: &RateLimitWindow,
) -> Result<TransactWriteItem, String> {
    let update = Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(window.partition.pk()))
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(action, &window.bucket)),
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
        .map_err(|error| format!("build RATELIMIT update: {error}"))?;

    Ok(TransactWriteItem::builder().update(update).build())
}

/// Indexes of the items whose condition failed in a canceled transaction.
/// Empty when the error is not a conditional cancellation at all.
pub fn conditional_failures(error: &SdkError<TransactWriteItemsError>) -> Vec<usize> {
    match error.as_service_error() {
        Some(TransactWriteItemsError::TransactionCanceledException(canceled)) => {
            failed_indexes(canceled)
        }
        _ => Vec::new(),
    }
}

fn failed_indexes(canceled: &TransactionCanceledException) -> Vec<usize> {
    canceled
        .cancellation_reasons()
        .iter()
        .enumerate()
        .filter(|(_, reason)| reason.code() == Some("ConditionalCheckFailed"))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::types::CancellationReason;
    use aws_sdk_dynamodb::types::error::TransactionCanceledException;

    use super::failed_indexes;

    fn canceled(reasons: Vec<CancellationReason>) -> TransactionCanceledException {
        TransactionCanceledException::builder()
            .set_cancellation_reasons(Some(reasons))
            .build()
    }

    fn failed() -> CancellationReason {
        CancellationReason::builder()
            .code("ConditionalCheckFailed")
            .build()
    }

    fn unrelated() -> CancellationReason {
        CancellationReason::builder()
            .code("TransactionConflict")
            .build()
    }

    #[test]
    fn reports_every_conditionally_failed_item() {
        assert_eq!(
            failed_indexes(&canceled(vec![failed(), unrelated(), failed()])),
            vec![0, 2]
        );
    }

    #[test]
    fn a_non_conditional_cancellation_has_no_failures() {
        assert!(failed_indexes(&canceled(vec![unrelated()])).is_empty());
    }
}
