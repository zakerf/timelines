//! TIMELINE items and their cover image (PRD §6 and §9).

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, boolean, check_item_type, get_bool,
    get_i64, get_map, get_opt_string, get_string, map, number, string,
};
use crate::keys;

/// The owner-uploaded hero image. Absent means the renderer emits the
/// deterministic procedural gradient instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverImage {
    /// S3 key; the renderer emits `/media/<key>`.
    pub key: String,
    pub width: u32,
    pub height: u32,
    /// Optional owner-typed attribution text, rendered as plain text.
    pub credit: Option<String>,
}

/// An ordered series of Events around one subject, owned by one Account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_id: String,
    pub collection_id: Option<String>,
    pub event_count: i64,
    pub upvotes: i64,
    pub downvotes: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub report_count: i64,
    pub flagged: bool,
    pub cover_image: Option<CoverImage>,
}

impl CoverImage {
    fn to_map(&self) -> AttrMap {
        let mut map = AttrMap::new();
        map.insert("key".to_owned(), string(&self.key));
        map.insert("width".to_owned(), number(i64::from(self.width)));
        map.insert("height".to_owned(), number(i64::from(self.height)));
        if let Some(credit) = &self.credit {
            map.insert("credit".to_owned(), string(credit));
        }
        map
    }

    fn from_map(map: &AttrMap) -> Result<Self, DecodeError> {
        let width = get_i64(map, "width")?;
        let height = get_i64(map, "height")?;
        Ok(Self {
            key: get_string(map, "key")?,
            width: u32::try_from(width).map_err(|_| DecodeError::InvalidValue {
                attribute: "width",
                value: width.to_string(),
            })?,
            height: u32::try_from(height).map_err(|_| DecodeError::InvalidValue {
                attribute: "height",
                value: height.to_string(),
            })?,
            credit: get_opt_string(map, "credit")?,
        })
    }
}

impl DynamoItem for Timeline {
    const ITEM_TYPE: ItemType = ItemType::Timeline;

    fn pk(&self) -> String {
        keys::timeline_pk(&self.id)
    }

    fn sk(&self) -> String {
        keys::TIMELINE_SK.to_owned()
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("title".to_owned(), string(&self.title));
        item.insert("description".to_owned(), string(&self.description));
        item.insert("ownerId".to_owned(), string(&self.owner_id));
        if let Some(collection_id) = &self.collection_id {
            item.insert("collectionId".to_owned(), string(collection_id));
        }
        item.insert("eventCount".to_owned(), number(self.event_count));
        item.insert("upvotes".to_owned(), number(self.upvotes));
        item.insert("downvotes".to_owned(), number(self.downvotes));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item.insert("updatedAt".to_owned(), string(self.updated_at.as_str()));
        item.insert("reportCount".to_owned(), number(self.report_count));
        item.insert("flagged".to_owned(), boolean(self.flagged));
        if let Some(cover) = &self.cover_image {
            item.insert("coverImage".to_owned(), map(cover.to_map()));
        }
        item.insert(
            "GSI2PK".to_owned(),
            string(keys::timeline_gsi2_pk(self.collection_id.as_deref())),
        );
        item.insert(
            "GSI2SK".to_owned(),
            string(keys::timeline_gsi2_sk(&self.created_at, &self.id)),
        );
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let cover_image = if item.contains_key("coverImage") {
            Some(CoverImage::from_map(&get_map(&item, "coverImage")?)?)
        } else {
            None
        };
        Ok(Self {
            id: keys::strip_timeline_pk(&get_string(&item, "PK")?)?,
            title: get_string(&item, "title")?,
            description: get_string(&item, "description")?,
            owner_id: get_string(&item, "ownerId")?,
            collection_id: get_opt_string(&item, "collectionId")?,
            event_count: get_i64(&item, "eventCount")?,
            upvotes: get_i64(&item, "upvotes")?,
            downvotes: get_i64(&item, "downvotes")?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
            updated_at: crate::item::get_timestamp(&item, "updatedAt")?,
            report_count: get_i64(&item, "reportCount")?,
            flagged: get_bool(&item, "flagged")?,
            cover_image,
        })
    }
}
