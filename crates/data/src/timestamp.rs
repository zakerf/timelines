//! Fixed-width UTC timestamps.
//!
//! Every sort key that embeds a timestamp uses the same canonical rendering:
//! `YYYY-MM-DDTHH:MM:SS.sssZ` (millisecond precision, always `Z`). Fixed width
//! is what makes lexicographic order match chronological order even when one
//! value has no fractional seconds and another does.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

/// A UTC ISO-8601 timestamp with millisecond precision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(String);

impl Timestamp {
    /// Current time.
    pub fn now() -> Self {
        Self(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    /// Parse an RFC 3339 timestamp and normalise it to UTC with milliseconds.
    pub fn parse(value: &str) -> Result<Self, TimestampError> {
        let parsed = DateTime::parse_from_rfc3339(value).map_err(|err| TimestampError {
            value: value.to_owned(),
            reason: err.to_string(),
        })?;
        Ok(Self(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
    }

    /// Midnight UTC on a date-only value (`YYYY-MM-DD`), matching PRD §6's
    /// rule that a date-only event gets `T00:00:00`.
    pub fn from_date(value: &str) -> Result<Self, TimestampError> {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|err| TimestampError {
            value: value.to_owned(),
            reason: err.to_string(),
        })?;
        let midnight = date.and_hms_opt(0, 0, 0).expect("midnight is always valid");
        Ok(Self(
            midnight
                .and_utc()
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
    }

    /// The canonical string, as stored in DynamoDB and embedded in keys.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Fixed-window bucket for per-minute limits: `YYYY-MM-DDTHH:MM`.
    pub fn minute_bucket(&self) -> String {
        self.0[..16].to_owned()
    }

    /// Fixed-window bucket for per-day limits: `YYYY-MM-DD`.
    pub fn date_bucket(&self) -> String {
        self.0[..10].to_owned()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A timestamp that could not be parsed or normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampError {
    value: String,
    reason: String,
}

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid UTC ISO-8601 timestamp {:?}: {}",
            self.value, self.reason
        )
    }
}

impl std::error::Error for TimestampError {}

/// Seconds since the Unix epoch, for the DynamoDB TTL attribute (`expiresAt`).
pub fn now_epoch_seconds() -> i64 {
    Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_rfc3339() {
        assert!(Timestamp::parse("2026-11-07").is_err());
        assert!(Timestamp::parse("not a timestamp").is_err());
    }

    #[test]
    fn normalises_offsets_to_utc() {
        assert_eq!(
            Timestamp::parse("2026-11-07T13:30:00+01:00")
                .unwrap()
                .as_str(),
            "2026-11-07T12:30:00.000Z"
        );
    }
}
