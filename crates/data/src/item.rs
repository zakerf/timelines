//! The DynamoDB attribute map and the conversions every item type shares.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use aws_sdk_dynamodb::types::AttributeValue;

use crate::Timestamp;

/// A DynamoDB item, as the SDK represents it.
pub type AttrMap = HashMap<String, AttributeValue>;

/// The `itemType` discriminator stored on every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Account,
    Collection,
    Timeline,
    Event,
    Vote,
    Suggestion,
    Report,
    RateLimit,
    SuggestCount,
    Session,
}

impl ItemType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Account => "ACCOUNT",
            Self::Collection => "COLLECTION",
            Self::Timeline => "TIMELINE",
            Self::Event => "EVENT",
            Self::Vote => "VOTE",
            Self::Suggestion => "SUGGESTION",
            Self::Report => "REPORT",
            Self::RateLimit => "RATELIMIT",
            Self::SuggestCount => "SUGGESTCOUNT",
            Self::Session => "SESSION",
        }
    }
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ItemType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ACCOUNT" => Ok(Self::Account),
            "COLLECTION" => Ok(Self::Collection),
            "TIMELINE" => Ok(Self::Timeline),
            "EVENT" => Ok(Self::Event),
            "VOTE" => Ok(Self::Vote),
            "SUGGESTION" => Ok(Self::Suggestion),
            "REPORT" => Ok(Self::Report),
            "RATELIMIT" => Ok(Self::RateLimit),
            "SUGGESTCOUNT" => Ok(Self::SuggestCount),
            "SESSION" => Ok(Self::Session),
            other => Err(format!("unknown item type {other:?}")),
        }
    }
}

/// A typed item that knows its single-table key layout.
pub trait DynamoItem: Sized {
    const ITEM_TYPE: ItemType;

    /// Partition key value.
    fn pk(&self) -> String;

    /// Sort key value.
    fn sk(&self) -> String;

    /// Full attribute map, including `itemType`, `PK`, and `SK`.
    fn to_item(&self) -> AttrMap;

    /// Parse an item back into its typed form.
    fn from_item(item: AttrMap) -> Result<Self, DecodeError>;
}

/// Failure to turn an attribute map into a typed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    MissingAttribute(&'static str),
    WrongType {
        attribute: &'static str,
        expected: &'static str,
    },
    UnexpectedItemType {
        expected: ItemType,
        actual: String,
    },
    InvalidValue {
        attribute: &'static str,
        value: String,
    },
    InvalidKey {
        key: &'static str,
        value: String,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAttribute(attribute) => {
                write!(f, "missing attribute {attribute:?}")
            }
            Self::WrongType {
                attribute,
                expected,
            } => write!(f, "attribute {attribute:?} is not {expected}"),
            Self::UnexpectedItemType { expected, actual } => {
                write!(f, "expected itemType {expected}, found {actual:?}")
            }
            Self::InvalidValue { attribute, value } => {
                write!(f, "attribute {attribute:?} has invalid value {value:?}")
            }
            Self::InvalidKey { key, value } => {
                write!(f, "key {key:?} has invalid value {value:?}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

pub(crate) fn base_item(item_type: ItemType, pk: String, sk: String) -> AttrMap {
    let mut item = AttrMap::new();
    item.insert(
        "itemType".to_owned(),
        AttributeValue::S(item_type.as_str().to_owned()),
    );
    item.insert("PK".to_owned(), AttributeValue::S(pk));
    item.insert("SK".to_owned(), AttributeValue::S(sk));
    item
}

pub(crate) fn check_item_type(item: &AttrMap, expected: ItemType) -> Result<(), DecodeError> {
    let actual = get_string(item, "itemType")?;
    if actual != expected.as_str() {
        return Err(DecodeError::UnexpectedItemType { expected, actual });
    }
    Ok(())
}

pub(crate) fn string(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

pub(crate) fn number(value: i64) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

pub(crate) fn boolean(value: bool) -> AttributeValue {
    AttributeValue::Bool(value)
}

pub(crate) fn string_list(values: &[String]) -> AttributeValue {
    AttributeValue::L(values.iter().map(|value| string(value.clone())).collect())
}

pub(crate) fn map(value: AttrMap) -> AttributeValue {
    AttributeValue::M(value)
}

pub(crate) fn get_string(item: &AttrMap, attribute: &'static str) -> Result<String, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::S(value)) => Ok(value.clone()),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a string",
        }),
        None => Err(DecodeError::MissingAttribute(attribute)),
    }
}

pub(crate) fn get_opt_string(
    item: &AttrMap,
    attribute: &'static str,
) -> Result<Option<String>, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::S(value)) => Ok(Some(value.clone())),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a string",
        }),
        None => Ok(None),
    }
}

pub(crate) fn get_i64(item: &AttrMap, attribute: &'static str) -> Result<i64, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::N(value)) => value.parse().map_err(|_| DecodeError::InvalidValue {
            attribute,
            value: value.clone(),
        }),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a number",
        }),
        None => Err(DecodeError::MissingAttribute(attribute)),
    }
}

pub(crate) fn get_bool(item: &AttrMap, attribute: &'static str) -> Result<bool, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::Bool(value)) => Ok(*value),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a boolean",
        }),
        None => Err(DecodeError::MissingAttribute(attribute)),
    }
}

pub(crate) fn get_string_list(
    item: &AttrMap,
    attribute: &'static str,
) -> Result<Vec<String>, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::L(values)) => values
            .iter()
            .map(|value| match value {
                AttributeValue::S(value) => Ok(value.clone()),
                _ => Err(DecodeError::WrongType {
                    attribute,
                    expected: "a list of strings",
                }),
            })
            .collect(),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a list of strings",
        }),
        None => Err(DecodeError::MissingAttribute(attribute)),
    }
}

pub(crate) fn get_opt_string_list(
    item: &AttrMap,
    attribute: &'static str,
) -> Result<Option<Vec<String>>, DecodeError> {
    if item.contains_key(attribute) {
        get_string_list(item, attribute).map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) fn get_timestamp(
    item: &AttrMap,
    attribute: &'static str,
) -> Result<Timestamp, DecodeError> {
    let value = get_string(item, attribute)?;
    Timestamp::parse(&value).map_err(|_| DecodeError::InvalidValue { attribute, value })
}

pub(crate) fn get_map(item: &AttrMap, attribute: &'static str) -> Result<AttrMap, DecodeError> {
    match item.get(attribute) {
        Some(AttributeValue::M(value)) => Ok(value.clone()),
        Some(_) => Err(DecodeError::WrongType {
            attribute,
            expected: "a map",
        }),
        None => Err(DecodeError::MissingAttribute(attribute)),
    }
}
