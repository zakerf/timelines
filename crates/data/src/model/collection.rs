//! COLLECTION items (PRD §6).

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, check_item_type, get_opt_string,
    get_string, string,
};
use crate::keys;

/// A navigational container for Timelines. Nesting is expressed by
/// `parent_collection_id`; the schema tolerates collections of collections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub description: String,
    pub parent_collection_id: Option<String>,
    pub created_at: Timestamp,
    pub created_by: String,
}

impl DynamoItem for Collection {
    const ITEM_TYPE: ItemType = ItemType::Collection;

    fn pk(&self) -> String {
        keys::collection_pk(&self.id)
    }

    fn sk(&self) -> String {
        keys::COLLECTION_SK.to_owned()
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("title".to_owned(), string(&self.title));
        item.insert("description".to_owned(), string(&self.description));
        if let Some(parent) = &self.parent_collection_id {
            item.insert("parentCollectionId".to_owned(), string(parent));
        }
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item.insert("createdBy".to_owned(), string(&self.created_by));
        item.insert(
            "GSI2PK".to_owned(),
            string(keys::collection_gsi2_pk(
                self.parent_collection_id.as_deref(),
            )),
        );
        item.insert(
            "GSI2SK".to_owned(),
            string(keys::collection_gsi2_sk(&self.id)),
        );
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        Ok(Self {
            id: keys::strip_collection_pk(&get_string(&item, "PK")?)?,
            title: get_string(&item, "title")?,
            description: get_string(&item, "description")?,
            parent_collection_id: get_opt_string(&item, "parentCollectionId")?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
            created_by: get_string(&item, "createdBy")?,
        })
    }
}
