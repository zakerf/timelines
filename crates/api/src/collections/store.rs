//! DynamoDB persistence for Collection CRUD and Timeline membership.
//!
//! Membership is adjacency: the TIMELINE row carries `collectionId` and the
//! matching GSI2 key attributes. Moving a Timeline is one `UpdateItem`, so the
//! GSI2 partition and the item can never disagree.

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use data::Timestamp;
use data::browse::{self, CollectionContents};
use data::item::DynamoItem;
use data::keys;
use data::model::{Collection, Timeline};

/// Store failure, kept as a message: handlers log it and return a coarse 500.
pub type StoreError = String;

fn s(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

pub async fn create_collection(
    client: &Client,
    table: &str,
    collection: &Collection,
) -> Result<(), StoreError> {
    client
        .put_item()
        .table_name(table)
        .set_item(Some(collection.to_item()))
        .condition_expression("attribute_not_exists(PK)")
        .send()
        .await
        .map_err(|error| format!("PutItem(COLLECTION) failed: {error:?}"))?;
    Ok(())
}

pub async fn get_collection(
    client: &Client,
    table: &str,
    id: &str,
) -> Result<Option<Collection>, StoreError> {
    browse::get_collection(client, table, id)
        .await
        .map_err(|error| format!("GetItem(COLLECTION) failed: {error}"))
}

pub async fn list_contents(
    client: &Client,
    table: &str,
    collection_id: Option<&str>,
) -> Result<CollectionContents, StoreError> {
    browse::list_contents(client, table, collection_id)
        .await
        .map_err(|error| format!("Query(GSI2) failed: {error}"))
}

pub async fn update_metadata(
    client: &Client,
    table: &str,
    id: &str,
    title: &str,
    description: &str,
    owner_id: &str,
) -> Result<(), StoreError> {
    client
        .update_item()
        .table_name(table)
        .key("PK", s(keys::collection_pk(id)))
        .key("SK", s(keys::COLLECTION_SK))
        .condition_expression("attribute_exists(PK) AND createdBy = :owner")
        .update_expression("SET title = :title, description = :description")
        .expression_attribute_values(":title", s(title))
        .expression_attribute_values(":description", s(description))
        .expression_attribute_values(":owner", s(owner_id))
        .send()
        .await
        .map_err(|error| format!("UpdateItem(COLLECTION) failed: {error:?}"))?;
    Ok(())
}

pub async fn delete_collection(
    client: &Client,
    table: &str,
    id: &str,
    owner_id: &str,
) -> Result<(), StoreError> {
    client
        .delete_item()
        .table_name(table)
        .key("PK", s(keys::collection_pk(id)))
        .key("SK", s(keys::COLLECTION_SK))
        .condition_expression("attribute_exists(PK) AND createdBy = :owner")
        .expression_attribute_values(":owner", s(owner_id))
        .send()
        .await
        .map_err(|error| format!("DeleteItem(COLLECTION) failed: {error:?}"))?;
    Ok(())
}

pub async fn get_timeline(
    client: &Client,
    table: &str,
    id: &str,
) -> Result<Option<Timeline>, StoreError> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", s(keys::timeline_pk(id)))
        .key("SK", s(keys::TIMELINE_SK))
        .send()
        .await
        .map_err(|error| format!("GetItem(TIMELINE) failed: {error:?}"))?;

    match output.item() {
        Some(item) => Timeline::from_item(item.clone())
            .map(Some)
            .map_err(|error| format!("could not decode TIMELINE: {error}")),
        None => Ok(None),
    }
}

/// Move a Timeline between collections, rewriting `collectionId` and the GSI2
/// keys in one conditional update. `None` removes it from any collection.
pub async fn set_timeline_collection(
    client: &Client,
    table: &str,
    timeline_id: &str,
    created_at: &Timestamp,
    collection_id: Option<&str>,
    owner_id: &str,
    now: &Timestamp,
) -> Result<(), StoreError> {
    let mut update_expression =
        String::from("SET GSI2PK = :gsi2pk, GSI2SK = :gsi2sk, updatedAt = :updated");
    let mut request = client
        .update_item()
        .table_name(table)
        .key("PK", s(keys::timeline_pk(timeline_id)))
        .key("SK", s(keys::TIMELINE_SK))
        .condition_expression("attribute_exists(PK) AND ownerId = :owner")
        .expression_attribute_values(":gsi2pk", s(keys::timeline_gsi2_pk(collection_id)))
        .expression_attribute_values(
            ":gsi2sk",
            s(keys::timeline_gsi2_sk(created_at, timeline_id)),
        )
        .expression_attribute_values(":updated", s(now.as_str()))
        .expression_attribute_values(":owner", s(owner_id));

    match collection_id {
        Some(id) => {
            update_expression.push_str(", collectionId = :collection");
            request = request.expression_attribute_values(":collection", s(id));
        }
        None => update_expression.push_str(" REMOVE collectionId"),
    }

    request
        .update_expression(update_expression)
        .send()
        .await
        .map_err(|error| format!("UpdateItem(TIMELINE membership) failed: {error:?}"))?;
    Ok(())
}
