//! Persistence for covers: the S3 object and the Timeline attribute.
//!
//! The object is immutable (a new key per upload), so replacing a cover is a
//! `PutObject` at a fresh key plus a `DeleteObject` of the superseded one. The
//! attribute write is conditioned on ownership; a failed attribute write makes
//! the caller delete the just-uploaded object so a failure does not leave an
//! obvious orphan.

use std::collections::HashMap;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_s3::primitives::ByteStream;
use data::Timestamp;
use data::keys;
use data::model::CoverImage;

use super::image::{CACHE_CONTROL, ImageFormat};

pub type StoreError = String;

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

pub async fn put_cover(
    client: &Client,
    table: &str,
    timeline_id: &str,
    owner_id: &str,
    cover: &CoverImage,
    updated_at: &Timestamp,
) -> Result<(), StoreError> {
    client
        .update_item()
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
        .send()
        .await
        .map(|_| ())
        .map_err(|error| format!("cover attribute update failed: {error:?}"))
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
}
