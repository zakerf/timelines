//! Persistence for covers: the S3 object and the Timeline attribute.
//!
//! The object is immutable (a new key per upload), so replacing a cover is a
//! `PutObject` at a fresh key plus a `DeleteObject` of the superseded one. The
//! attribute write is conditioned on ownership; a failed attribute write makes
//! the caller delete the just-uploaded object so a failure does not leave an
//! obvious orphan.
//!
//! A cover upload rides the publish/append limit (PRD §8, §9): one
//! `publish-event` counter increment in the same `TransactWriteItems` as the
//! attribute write, so a refused upload never leaves a partial row or a
//! consumed counter.

use std::collections::HashMap;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, TransactWriteItem, Update};
use aws_sdk_s3::primitives::ByteStream;
use data::Timestamp;
use data::keys;
use data::model::CoverImage;

use super::image::{CACHE_CONTROL, ImageFormat};
use crate::publish::{EVENT_LIMIT_ACTION, EVENTS_PER_DAY};

pub type StoreError = String;

/// Keep a window's counter one day past its bucket so the TTL never races a
/// still-current window (matches the publish store's TTL).
const LIMIT_TTL_SECONDS: i64 = 2 * 86_400;

pub async fn put_media(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    bytes: Vec<u8>,
    format: ImageFormat,
) -> Result<(), StoreError> {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_type(format.content_type())
        .cache_control(CACHE_CONTROL)
        .body(ByteStream::from(bytes))
        .send()
        .await
        .map(|_| ())
        .map_err(|error| format!("PutObject({key}) failed: {error:?}"))
}

/// Best-effort delete: a superseded object is an accepted orphan if S3 refuses
/// it, never a reason to fail the request that already committed the new key.
pub async fn delete_media(client: &aws_sdk_s3::Client, bucket: &str, key: &str) {
    if let Err(error) = client.delete_object().bucket(bucket).key(key).send().await {
        tracing::warn!(%key, %error, "superseded cover object was not deleted");
    }
}

/// A refusal the cover attribute transaction can produce.
#[derive(Debug)]
pub enum CoverWriteError {
    /// The account's `publish-event` window is full (PRD §8).
    RateLimited { retry_after_seconds: u64 },
    Store(String),
}

/// Write the `coverImage` attribute and consume the publish/append counter in
/// one transaction. The attribute write stays conditioned on ownership, so a
/// stolen session cannot overwrite a cover even past the handler's check.
pub async fn put_cover(
    client: &Client,
    table: &str,
    timeline_id: &str,
    owner_id: &str,
    cover: &CoverImage,
    updated_at: &Timestamp,
) -> Result<(), CoverWriteError> {
    let bucket = updated_at.date_bucket();
    let now_epoch = data::timestamp::now_epoch_seconds();

    let rate_limit = rate_limit_write(table, owner_id, &bucket, now_epoch + LIMIT_TTL_SECONDS)?;
    let cover_update = cover_update(table, timeline_id, owner_id, cover, updated_at)?;

    let writes = vec![
        TransactWriteItem::builder().update(rate_limit).build(),
        TransactWriteItem::builder().update(cover_update).build(),
    ];

    match client
        .transact_write_items()
        .set_transact_items(Some(writes))
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            if let Some(TransactWriteItemsError::TransactionCanceledException(canceled)) =
                error.as_service_error()
            {
                // The counter leads the write list; a full window cancels there.
                let limit_refused = canceled
                    .cancellation_reasons()
                    .first()
                    .is_some_and(|reason| reason.code() == Some("ConditionalCheckFailed"));
                if limit_refused {
                    return Err(CoverWriteError::RateLimited {
                        retry_after_seconds: seconds_until_day_end(now_epoch),
                    });
                }
            }
            Err(CoverWriteError::Store(format!(
                "cover attribute transaction failed: {error:?}"
            )))
        }
    }
}

pub async fn remove_cover(
    client: &Client,
    table: &str,
    timeline_id: &str,
    owner_id: &str,
    updated_at: &Timestamp,
) -> Result<(), StoreError> {
    client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .condition_expression("attribute_exists(PK) AND ownerId = :owner")
        .update_expression("SET updatedAt = :updated REMOVE coverImage")
        .expression_attribute_values(":owner", AttributeValue::S(owner_id.to_owned()))
        .expression_attribute_values(
            ":updated",
            AttributeValue::S(updated_at.as_str().to_owned()),
        )
        .send()
        .await
        .map(|_| ())
        .map_err(|error| format!("cover attribute removal failed: {error:?}"))
}

/// The `publish-event` fixed-window counter under `RL#ACCOUNT#<ownerId>`,
/// incremented by one and refused once the day's budget is full.
fn rate_limit_write(
    table: &str,
    owner_id: &str,
    bucket: &str,
    expires_at: i64,
) -> Result<Update, CoverWriteError> {
    Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::ratelimit_account_pk(owner_id)))
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(EVENT_LIMIT_ACTION, bucket)),
        )
        .update_expression(
            "SET #itemType = if_not_exists(#itemType, :itemType), \
             #expiresAt = if_not_exists(#expiresAt, :expiresAt) ADD #count :one",
        )
        .condition_expression("attribute_not_exists(#count) OR #count < :limit")
        .expression_attribute_names("#itemType", "itemType")
        .expression_attribute_names("#expiresAt", "expiresAt")
        .expression_attribute_names("#count", "count")
        .expression_attribute_values(":itemType", AttributeValue::S("RATELIMIT".to_owned()))
        .expression_attribute_values(":expiresAt", AttributeValue::N(expires_at.to_string()))
        .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
        .expression_attribute_values(":limit", AttributeValue::N(EVENTS_PER_DAY.to_string()))
        .build()
        .map_err(|error| CoverWriteError::Store(format!("build RATELIMIT update: {error}")))
}

fn cover_update(
    table: &str,
    timeline_id: &str,
    owner_id: &str,
    cover: &CoverImage,
    updated_at: &Timestamp,
) -> Result<Update, CoverWriteError> {
    Update::builder()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .condition_expression("attribute_exists(PK) AND ownerId = :owner")
        .update_expression("SET coverImage = :cover, updatedAt = :updated")
        .expression_attribute_values(":owner", AttributeValue::S(owner_id.to_owned()))
        .expression_attribute_values(":cover", cover_attribute(cover))
        .expression_attribute_values(
            ":updated",
            AttributeValue::S(updated_at.as_str().to_owned()),
        )
        .build()
        .map_err(|error| CoverWriteError::Store(format!("build cover update: {error}")))
}

/// Seconds left in the current UTC day, for `Retry-After`.
fn seconds_until_day_end(now_epoch: i64) -> u64 {
    (86_400 - now_epoch.rem_euclid(86_400)).max(1) as u64
}

fn cover_attribute(cover: &CoverImage) -> AttributeValue {
    let mut map = HashMap::new();
    map.insert("key".to_owned(), AttributeValue::S(cover.key.clone()));
    map.insert(
        "width".to_owned(),
        AttributeValue::N(cover.width.to_string()),
    );
    map.insert(
        "height".to_owned(),
        AttributeValue::N(cover.height.to_string()),
    );
    if let Some(credit) = &cover.credit {
        map.insert("credit".to_owned(), AttributeValue::S(credit.clone()));
    }
    AttributeValue::M(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attribute_matches_the_data_model_shape() {
        let attribute = cover_attribute(&CoverImage {
            key: "cover/t1/abc.png".to_owned(),
            width: 1600,
            height: 900,
            credit: Some("Photo: Jane".to_owned()),
        });

        let AttributeValue::M(map) = attribute else {
            panic!("coverImage is a map");
        };
        assert_eq!(
            map.get("key"),
            Some(&AttributeValue::S("cover/t1/abc.png".to_owned()))
        );
        assert_eq!(
            map.get("width"),
            Some(&AttributeValue::N("1600".to_owned()))
        );
        assert_eq!(
            map.get("height"),
            Some(&AttributeValue::N("900".to_owned()))
        );
        assert_eq!(
            map.get("credit"),
            Some(&AttributeValue::S("Photo: Jane".to_owned()))
        );
    }

    #[test]
    fn retry_after_runs_to_utc_midnight() {
        // 12:34:56 UTC -> 11h 25m 4s left.
        let epoch = 12 * 3600 + 34 * 60 + 56;
        assert_eq!(seconds_until_day_end(epoch), 86_400 - (12 * 3600 + 34 * 60 + 56));
        assert_eq!(seconds_until_day_end(0), 86_400);
    }
}
