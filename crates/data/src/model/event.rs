//! EVENT items (PRD §6).

use std::fmt;
use std::str::FromStr;

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, boolean, check_item_type, get_bool,
    get_i64, get_opt_string, get_string, get_string_list, number, string, string_list,
};
use crate::keys;

/// How much an Event matters to the reader-facing story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Significance {
    Minor,
    #[default]
    Regular,
    Crucial,
}

impl Significance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Regular => "regular",
            Self::Crucial => "crucial",
        }
    }
}

impl fmt::Display for Significance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Significance {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "minor" => Ok(Self::Minor),
            "regular" | "standard" => Ok(Self::Regular),
            "crucial" | "featured" => Ok(Self::Crucial),
            other => Err(format!("unknown significance {other:?}")),
        }
    }
}

/// One dated fact inside a Timeline. `timestamp` is the SK component (full
/// date-time UTC); `date` is the reader-facing exact date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub timeline_id: String,
    pub event_id: String,
    pub timestamp: Timestamp,
    pub date: String,
    pub title: String,
    pub description: String,
    pub sources: Vec<String>,
    pub significance: Significance,
    pub upvotes: i64,
    pub downvotes: i64,
    pub report_count: i64,
    pub flagged: bool,
    pub created_at: Timestamp,
    pub created_by: String,
    pub last_edited_by: Option<String>,
    pub last_edit_suggestion_id: Option<String>,
}

impl DynamoItem for Event {
    const ITEM_TYPE: ItemType = ItemType::Event;

    fn pk(&self) -> String {
        keys::event_pk(&self.timeline_id)
    }

    fn sk(&self) -> String {
        keys::event_sk(&self.timestamp, &self.event_id)
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("date".to_owned(), string(&self.date));
        item.insert("title".to_owned(), string(&self.title));
        item.insert("description".to_owned(), string(&self.description));
        item.insert("sources".to_owned(), string_list(&self.sources));
        item.insert(
            "significance".to_owned(),
            string(self.significance.as_str()),
        );
        item.insert("upvotes".to_owned(), number(self.upvotes));
        item.insert("downvotes".to_owned(), number(self.downvotes));
        item.insert("reportCount".to_owned(), number(self.report_count));
        item.insert("flagged".to_owned(), boolean(self.flagged));
        item.insert("createdAt".to_owned(), string(self.created_at.as_str()));
        item.insert("createdBy".to_owned(), string(&self.created_by));
        if let Some(editor) = &self.last_edited_by {
            item.insert("lastEditedBy".to_owned(), string(editor));
        }
        if let Some(suggestion_id) = &self.last_edit_suggestion_id {
            item.insert("lastEditSuggestionId".to_owned(), string(suggestion_id));
        }
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let (timestamp, event_id) = keys::strip_event_sk(&get_string(&item, "SK")?)?;
        let significance_value = match get_opt_string(&item, "significance")? {
            Some(value) => value,
            // Rows written before the Emphasis -> Significance rename.
            None => get_opt_string(&item, "emphasis")?
                .ok_or(DecodeError::MissingAttribute("significance"))?,
        };
        let significance = significance_value
            .parse()
            .map_err(|_| DecodeError::InvalidValue {
                attribute: "significance",
                value: significance_value,
            })?;
        Ok(Self {
            timeline_id: keys::strip_timeline_pk(&get_string(&item, "PK")?)?,
            event_id,
            timestamp,
            date: get_string(&item, "date")?,
            title: get_string(&item, "title")?,
            description: get_string(&item, "description")?,
            sources: get_string_list(&item, "sources")?,
            significance,
            upvotes: get_i64(&item, "upvotes")?,
            downvotes: get_i64(&item, "downvotes")?,
            report_count: get_i64(&item, "reportCount")?,
            flagged: get_bool(&item, "flagged")?,
            created_at: crate::item::get_timestamp(&item, "createdAt")?,
            created_by: get_string(&item, "createdBy")?,
            last_edited_by: get_opt_string(&item, "lastEditedBy")?,
            last_edit_suggestion_id: get_opt_string(&item, "lastEditSuggestionId")?,
        })
    }
}
