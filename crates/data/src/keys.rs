//! Exact partition/sort key builders from PRD §6.
//!
//! These are the only places key strings are assembled, so the access patterns
//! in the PRD map one-to-one onto code.

use crate::Timestamp;
use crate::item::DecodeError;
use crate::model::{RateLimitPartition, ReportTargetType};

/// Sort key of the account profile item.
pub const ACCOUNT_SK: &str = "PROFILE";
/// Sort key of collection, timeline, and session metadata items.
pub const META_SK: &str = "META";
/// Sort key shared by collection metadata.
pub const COLLECTION_SK: &str = "META";
/// Sort key shared by timeline metadata.
pub const TIMELINE_SK: &str = "META";
/// Sort key shared by session metadata.
pub const SESSION_SK: &str = "META";
/// Sort key of a short-lived OIDC authorization-code state item.
pub const AUTH_STATE_SK: &str = "PKCE";
/// Prefix of event sort keys inside a timeline partition.
pub const EVENT_SK_PREFIX: &str = "EVENT#";
/// Prefix of suggestion sort keys inside a timeline partition.
pub const SUGGESTION_SK_PREFIX: &str = "SUGGESTION#";
/// GSI2 partition for collections/timelines with no parent or collection.
pub const ROOT_COLLECTION: &str = "ROOT";

/// `ACCOUNT#<sub>`
pub fn account_pk(sub: &str) -> String {
    format!("ACCOUNT#{sub}")
}

/// `COLLECTION#<id>`
pub fn collection_pk(id: &str) -> String {
    format!("COLLECTION#{id}")
}

/// `TIMELINE#<id>`
pub fn timeline_pk(id: &str) -> String {
    format!("TIMELINE#{id}")
}

/// Events live in their timeline's partition: `TIMELINE#<timelineId>`.
pub fn event_pk(timeline_id: &str) -> String {
    timeline_pk(timeline_id)
}

/// `EVENT#<timestamp>#<eventId>`
pub fn event_sk(timestamp: &Timestamp, event_id: &str) -> String {
    format!("{EVENT_SK_PREFIX}{timestamp}#{event_id}")
}

/// `VOTE#<accountId>`
pub fn vote_pk(account_id: &str) -> String {
    format!("VOTE#{account_id}")
}

/// Timeline-level vote: `T#<timelineId>`.
pub fn timeline_vote_sk(timeline_id: &str) -> String {
    format!("T#{timeline_id}")
}

/// Event-level vote: `T#<timelineId>#EVENT#<eventId>`.
pub fn event_vote_sk(timeline_id: &str, event_id: &str) -> String {
    format!("T#{timeline_id}#EVENT#{event_id}")
}

/// Prefix for "every vote this account holds on one timeline".
///
/// This deliberately has no trailing `#`: it matches the timeline-level sort
/// key exactly and both vote levels as a prefix.
pub fn vote_timeline_prefix(timeline_id: &str) -> String {
    format!("T#{timeline_id}")
}

/// Suggestions live in the timeline partition: `TIMELINE#<timelineId>`.
pub fn suggestion_pk(timeline_id: &str) -> String {
    timeline_pk(timeline_id)
}

/// `SUGGESTION#<suggestionId>`
pub fn suggestion_sk(suggestion_id: &str) -> String {
    format!("{SUGGESTION_SK_PREFIX}{suggestion_id}")
}

/// Sparse GSI1 partition for the owner's pending queue:
/// `OWNER#<ownerId>#PENDING`.
pub fn suggestion_gsi1_pk(owner_id: &str) -> String {
    format!("OWNER#{owner_id}#PENDING")
}

/// `REPORT#<targetType>#<targetId>`
pub fn report_pk(target_type: ReportTargetType, target_id: &str) -> String {
    format!("REPORT#{}#{target_id}", target_type.as_str())
}

/// `ACCOUNT#<accountId>`
pub fn report_sk(account_id: &str) -> String {
    format!("ACCOUNT#{account_id}")
}

/// Per-account rate-limit partition: `RL#ACCOUNT#<accountId>`.
pub fn ratelimit_account_pk(account_id: &str) -> String {
    format!("RL#ACCOUNT#{account_id}")
}

/// Per-IP rate-limit partition: `RL#IP#<sha256(clientIp + salt)>`.
pub fn ratelimit_ip_pk(ip_hash: &str) -> String {
    format!("RL#IP#{ip_hash}")
}

/// `<action>#<windowBucket>`
pub fn ratelimit_sk(action: &str, window_bucket: &str) -> String {
    format!("{action}#{window_bucket}")
}

/// Per-account pending-suggestion counter lives in the timeline partition.
pub fn suggestcount_pk(timeline_id: &str) -> String {
    timeline_pk(timeline_id)
}

/// `SUGGESTCOUNT#<accountId>`
pub fn suggestcount_sk(account_id: &str) -> String {
    format!("SUGGESTCOUNT#{account_id}")
}

/// `SESSION#<id>`
pub fn session_pk(id: &str) -> String {
    format!("SESSION#{id}")
}

/// `AUTHSTATE#<state>` -- the server-side half of the PKCE handshake.
pub fn auth_state_pk(state: &str) -> String {
    format!("AUTHSTATE#{state}")
}

/// GSI2 partition for collection membership: the parent collection id, or
/// `ROOT` for top-level collections.
pub fn collection_gsi2_pk(parent_collection_id: Option<&str>) -> String {
    parent_collection_id.unwrap_or(ROOT_COLLECTION).to_owned()
}

/// `C#<id>`
pub fn collection_gsi2_sk(id: &str) -> String {
    format!("C#{id}")
}

/// GSI2 partition for timeline membership: the collection id, or `ROOT`.
pub fn timeline_gsi2_pk(collection_id: Option<&str>) -> String {
    collection_id.unwrap_or(ROOT_COLLECTION).to_owned()
}

/// `T#<createdAt>#<id>`
pub fn timeline_gsi2_sk(created_at: &Timestamp, id: &str) -> String {
    format!("T#{created_at}#{id}")
}

fn strip<'a>(key: &'static str, value: &'a str, prefix: &str) -> Result<&'a str, DecodeError> {
    value
        .strip_prefix(prefix)
        .ok_or_else(|| DecodeError::InvalidKey {
            key,
            value: value.to_owned(),
        })
}

pub(crate) fn strip_account_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "ACCOUNT#")?.to_owned())
}

pub(crate) fn strip_collection_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "COLLECTION#")?.to_owned())
}

pub(crate) fn strip_timeline_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "TIMELINE#")?.to_owned())
}

pub(crate) fn strip_event_sk(sk: &str) -> Result<(Timestamp, String), DecodeError> {
    let rest = strip("SK", sk, EVENT_SK_PREFIX)?;
    let (timestamp, event_id) = rest
        .split_once('#')
        .ok_or_else(|| DecodeError::InvalidKey {
            key: "SK",
            value: sk.to_owned(),
        })?;
    let timestamp = Timestamp::parse(timestamp).map_err(|_| DecodeError::InvalidKey {
        key: "SK",
        value: sk.to_owned(),
    })?;
    Ok((timestamp, event_id.to_owned()))
}

pub(crate) fn strip_vote_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "VOTE#")?.to_owned())
}

pub(crate) fn strip_vote_sk(sk: &str) -> Result<(String, Option<String>), DecodeError> {
    let rest = strip("SK", sk, "T#")?;
    match rest.split_once("#EVENT#") {
        Some((timeline_id, event_id)) => Ok((timeline_id.to_owned(), Some(event_id.to_owned()))),
        None => Ok((rest.to_owned(), None)),
    }
}

pub(crate) fn strip_suggestion_sk(sk: &str) -> Result<String, DecodeError> {
    Ok(strip("SK", sk, SUGGESTION_SK_PREFIX)?.to_owned())
}

pub(crate) fn strip_report_pk(pk: &str) -> Result<(ReportTargetType, String), DecodeError> {
    let rest = strip("PK", pk, "REPORT#")?;
    let (target_type, target_id) = rest
        .split_once('#')
        .ok_or_else(|| DecodeError::InvalidKey {
            key: "PK",
            value: pk.to_owned(),
        })?;
    let target_type = target_type.parse().map_err(|_| DecodeError::InvalidKey {
        key: "PK",
        value: pk.to_owned(),
    })?;
    Ok((target_type, target_id.to_owned()))
}

pub(crate) fn strip_report_sk(sk: &str) -> Result<String, DecodeError> {
    Ok(strip("SK", sk, "ACCOUNT#")?.to_owned())
}

pub(crate) fn strip_ratelimit_pk(pk: &str) -> Result<RateLimitPartition, DecodeError> {
    if let Some(account_id) = pk.strip_prefix("RL#ACCOUNT#") {
        Ok(RateLimitPartition::Account(account_id.to_owned()))
    } else if let Some(hash) = pk.strip_prefix("RL#IP#") {
        Ok(RateLimitPartition::IpHash(hash.to_owned()))
    } else {
        Err(DecodeError::InvalidKey {
            key: "PK",
            value: pk.to_owned(),
        })
    }
}

pub(crate) fn strip_ratelimit_sk(sk: &str) -> Result<(String, String), DecodeError> {
    let (action, window_bucket) = sk.split_once('#').ok_or_else(|| DecodeError::InvalidKey {
        key: "SK",
        value: sk.to_owned(),
    })?;
    Ok((action.to_owned(), window_bucket.to_owned()))
}

pub(crate) fn strip_suggestcount_sk(sk: &str) -> Result<String, DecodeError> {
    Ok(strip("SK", sk, "SUGGESTCOUNT#")?.to_owned())
}

pub(crate) fn strip_session_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "SESSION#")?.to_owned())
}

pub(crate) fn strip_auth_state_pk(pk: &str) -> Result<String, DecodeError> {
    Ok(strip("PK", pk, "AUTHSTATE#")?.to_owned())
}
