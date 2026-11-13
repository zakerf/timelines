//! SUGGESTION items, payload shapes, and the sparse owner-queue index.

use std::fmt;
use std::str::FromStr;

use crate::Timestamp;
use crate::item::{
    AttrMap, DecodeError, DynamoItem, ItemType, base_item, boolean, check_item_type, get_bool_or,
    get_i64_or, get_map, get_opt_string, get_opt_string_list, get_string, get_string_list, map,
    number, string, string_list,
};
use crate::keys;
use crate::model::Emphasis;

/// Where a suggestion sits in the owner's moderation flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestionState {
    Pending,
    Approved,
    Rejected,
}

impl SuggestionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
        }
    }
}

impl fmt::Display for SuggestionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SuggestionState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PENDING" => Ok(Self::Pending),
            "APPROVED" => Ok(Self::Approved),
            "REJECTED" => Ok(Self::Rejected),
            other => Err(format!("unknown suggestion state {other:?}")),
        }
    }
}

/// The two suggestion shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestionKind {
    Append,
    Edit,
}

impl SuggestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "APPEND",
            Self::Edit => "EDIT",
        }
    }
}

impl fmt::Display for SuggestionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SuggestionKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "APPEND" => Ok(Self::Append),
            "EDIT" => Ok(Self::Edit),
            other => Err(format!("unknown suggestion kind {other:?}")),
        }
    }
}

/// A proposed new Event: the full event fields, without ids or timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendPayload {
    pub date: String,
    pub title: String,
    pub description: String,
    pub sources: Vec<String>,
    pub emphasis: Emphasis,
}

/// A proposed change to an existing Event. Only the fields the author wants
/// to change are present; `None` means "leave untouched".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditPayload {
    pub title: Option<String>,
    pub description: Option<String>,
    pub date: Option<String>,
    pub sources: Option<Vec<String>>,
    pub emphasis: Option<Emphasis>,
}

/// The sparse `payload` map. Which variant applies is fixed by `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionPayload {
    Append(AppendPayload),
    Edit(EditPayload),
}

impl SuggestionPayload {
    fn to_map(&self) -> AttrMap {
        let mut map = AttrMap::new();
        match self {
            Self::Append(payload) => {
                map.insert("date".to_owned(), string(&payload.date));
                map.insert("title".to_owned(), string(&payload.title));
                map.insert("description".to_owned(), string(&payload.description));
                map.insert("sources".to_owned(), string_list(&payload.sources));
                map.insert("emphasis".to_owned(), string(payload.emphasis.as_str()));
            }
            Self::Edit(payload) => {
                if let Some(title) = &payload.title {
                    map.insert("title".to_owned(), string(title));
                }
                if let Some(description) = &payload.description {
                    map.insert("description".to_owned(), string(description));
                }
                if let Some(date) = &payload.date {
                    map.insert("date".to_owned(), string(date));
                }
                if let Some(sources) = &payload.sources {
                    map.insert("sources".to_owned(), string_list(sources));
                }
                if let Some(emphasis) = payload.emphasis {
                    map.insert("emphasis".to_owned(), string(emphasis.as_str()));
                }
            }
        }
        map
    }

    fn from_map(kind: SuggestionKind, map: &AttrMap) -> Result<Self, DecodeError> {
        match kind {
            SuggestionKind::Append => Ok(Self::Append(AppendPayload {
                date: get_string(map, "date")?,
                title: get_string(map, "title")?,
                description: get_string(map, "description")?,
                sources: get_string_list(map, "sources")?,
                emphasis: parse_emphasis(map, "emphasis")?
                    .ok_or(DecodeError::MissingAttribute("emphasis"))?,
            })),
            SuggestionKind::Edit => Ok(Self::Edit(EditPayload {
                title: get_opt_string(map, "title")?,
                description: get_opt_string(map, "description")?,
                date: get_opt_string(map, "date")?,
                sources: get_opt_string_list(map, "sources")?,
                emphasis: parse_emphasis(map, "emphasis")?,
            })),
        }
    }
}

fn parse_emphasis(map: &AttrMap, attribute: &'static str) -> Result<Option<Emphasis>, DecodeError> {
    match get_opt_string(map, attribute)? {
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| DecodeError::InvalidValue { attribute, value }),
        None => Ok(None),
    }
}

/// A proposed change to a Timeline's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub suggestion_id: String,
    pub timeline_id: String,
    /// Timeline owner. Not a stored attribute: it exists only to derive the
    /// sparse GSI1 queue key at submit time, so reads come back with `None`.
    pub owner_id: Option<String>,
    pub state: SuggestionState,
    pub kind: SuggestionKind,
    /// `EDIT` only.
    pub target_event_id: Option<String>,
    pub payload: SuggestionPayload,
    pub submitted_by: String,
    pub submitted_at: Timestamp,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<Timestamp>,
    pub review_note: Option<String>,
    /// Reports targeting this Suggestion (PRD §8). Additive to the item.
    pub report_count: i64,
    /// Internal flag at three distinct reporting accounts (PRD §8).
    pub flagged: bool,
}

impl DynamoItem for Suggestion {
    const ITEM_TYPE: ItemType = ItemType::Suggestion;

    fn pk(&self) -> String {
        keys::suggestion_pk(&self.timeline_id)
    }

    fn sk(&self) -> String {
        keys::suggestion_sk(&self.suggestion_id)
    }

    fn to_item(&self) -> AttrMap {
        let mut item = base_item(Self::ITEM_TYPE, self.pk(), self.sk());
        item.insert("state".to_owned(), string(self.state.as_str()));
        item.insert("kind".to_owned(), string(self.kind.as_str()));
        if self.kind == SuggestionKind::Edit
            && let Some(target_event_id) = &self.target_event_id
        {
            item.insert("targetEventId".to_owned(), string(target_event_id));
        }
        item.insert("payload".to_owned(), map(self.payload.to_map()));
        item.insert("submittedBy".to_owned(), string(&self.submitted_by));
        item.insert("submittedAt".to_owned(), string(self.submitted_at.as_str()));
        if let Some(reviewed_by) = &self.reviewed_by {
            item.insert("reviewedBy".to_owned(), string(reviewed_by));
        }
        if let Some(reviewed_at) = &self.reviewed_at {
            item.insert("reviewedAt".to_owned(), string(reviewed_at.as_str()));
        }
        if let Some(review_note) = &self.review_note {
            item.insert("reviewNote".to_owned(), string(review_note));
        }
        item.insert("reportCount".to_owned(), number(self.report_count));
        item.insert("flagged".to_owned(), boolean(self.flagged));
        // Sparse owner queue: present only while pending, removed by the same
        // write that flips state (PRD §6).
        if self.state == SuggestionState::Pending
            && let Some(owner_id) = &self.owner_id
        {
            item.insert(
                "GSI1PK".to_owned(),
                string(keys::suggestion_gsi1_pk(owner_id)),
            );
            item.insert("GSI1SK".to_owned(), string(self.submitted_at.as_str()));
        }
        item
    }

    fn from_item(item: AttrMap) -> Result<Self, DecodeError> {
        check_item_type(&item, Self::ITEM_TYPE)?;
        let state_value = get_string(&item, "state")?;
        let state: SuggestionState =
            state_value.parse().map_err(|_| DecodeError::InvalidValue {
                attribute: "state",
                value: state_value,
            })?;
        let kind_value = get_string(&item, "kind")?;
        let kind: SuggestionKind = kind_value.parse().map_err(|_| DecodeError::InvalidValue {
            attribute: "kind",
            value: kind_value,
        })?;
        let payload = SuggestionPayload::from_map(kind, &get_map(&item, "payload")?)?;
        Ok(Self {
            suggestion_id: keys::strip_suggestion_sk(&get_string(&item, "SK")?)?,
            timeline_id: keys::strip_timeline_pk(&get_string(&item, "PK")?)?,
            owner_id: None,
            state,
            kind,
            target_event_id: get_opt_string(&item, "targetEventId")?,
            payload,
            submitted_by: get_string(&item, "submittedBy")?,
            submitted_at: crate::item::get_timestamp(&item, "submittedAt")?,
            reviewed_by: get_opt_string(&item, "reviewedBy")?,
            reviewed_at: match get_opt_string(&item, "reviewedAt")? {
                Some(value) => {
                    Some(
                        Timestamp::parse(&value).map_err(|_| DecodeError::InvalidValue {
                            attribute: "reviewedAt",
                            value,
                        })?,
                    )
                }
                None => None,
            },
            review_note: get_opt_string(&item, "reviewNote")?,
            report_count: get_i64_or(&item, "reportCount", 0)?,
            flagged: get_bool_or(&item, "flagged", false)?,
        })
    }
}
