//! Collection browsing reads (PRD §6, access pattern 4).
//!
//! Membership lives entirely in GSI2: every COLLECTION and TIMELINE item
//! carries `GSI2PK = parentCollectionId ?? collectionId ?? "ROOT"` and a
//! type-prefixed `GSI2SK`. One query per collection partition therefore returns
//! its child collections and its Timelines together. Nesting is adjacency
//! (`parentCollectionId`), so a subtree is a walk — not a materialized path.

use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::io;
use std::pin::Pin;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;

use crate::item::{AttrMap, DynamoItem, ItemType};
use crate::keys;
use crate::model::{Collection, Timeline};

/// Read failure, kept as an opaque boxed error like the renderer's read path.
pub type BrowseResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// The direct contents of one collection partition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionContents {
    /// Child collections, in `GSI2SK` (`C#<id>`) order.
    pub collections: Vec<Collection>,
    /// Timelines directly assigned to this collection, in `GSI2SK`
    /// (`T#<createdAt>#<id>`) order.
    pub timelines: Vec<Timeline>,
}

/// One node of a recursive adjacency walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionSubtree {
    pub collection: Collection,
    /// Timelines directly assigned to this collection (not descendants).
    pub timelines: Vec<Timeline>,
    pub children: Vec<CollectionSubtree>,
}

/// List a collection's direct members. `None` is the root (top level).
pub async fn list_contents(
    client: &Client,
    table: &str,
    collection_id: Option<&str>,
) -> BrowseResult<CollectionContents> {
    let partition = keys::collection_gsi2_pk(collection_id);
    let mut contents = CollectionContents::default();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .index_name("GSI2")
            .key_condition_expression("GSI2PK = :pk")
            .expression_attribute_values(":pk", AttributeValue::S(partition.clone()));
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request.send().await?;

        for item in output.items() {
            match item_type(item)? {
                ItemType::Collection => contents
                    .collections
                    .push(Collection::from_item(item.clone())?),
                ItemType::Timeline => contents.timelines.push(Timeline::from_item(item.clone())?),
                _ => {}
            }
        }

        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    Ok(contents)
}

/// Get one COLLECTION metadata row.
pub async fn get_collection(
    client: &Client,
    table: &str,
    id: &str,
) -> BrowseResult<Option<Collection>> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::collection_pk(id)))
        .key("SK", AttributeValue::S(keys::COLLECTION_SK.to_owned()))
        .send()
        .await?;

    match output.item() {
        Some(item) => Ok(Some(Collection::from_item(item.clone())?)),
        None => Ok(None),
    }
}

/// Walk a collection's adjacency pointers depth-first and return the subtree.
///
/// The async recursion is boxed explicitly; collections are small and shallow,
/// so the walk stays one query per collection.
pub fn collection_subtree<'a>(
    client: &'a Client,
    table: &'a str,
    root_id: &'a str,
) -> Pin<Box<dyn Future<Output = BrowseResult<CollectionSubtree>> + Send + 'a>> {
    Box::pin(async move {
        let collection = get_collection(client, table, root_id)
            .await?
            .ok_or_else(|| not_found(format!("collection {root_id:?} does not exist")))?;
        let contents = list_contents(client, table, Some(root_id)).await?;

        let mut children = Vec::with_capacity(contents.collections.len());
        for child in &contents.collections {
            children.push(collection_subtree(client, table, &child.id).await?);
        }

        Ok(CollectionSubtree {
            collection,
            timelines: contents.timelines,
            children,
        })
    })
}

fn item_type(item: &AttrMap) -> BrowseResult<ItemType> {
    let raw = match item.get("itemType") {
        Some(AttributeValue::S(value)) => value,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GSI2 row has no itemType discriminator",
            )
            .into());
        }
    };
    raw.parse::<ItemType>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error).into())
}

fn not_found(message: String) -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(io::ErrorKind::NotFound, message))
}
