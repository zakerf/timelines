//! Fixed-window vote limits (PRD §8).
//!
//! Every vote — cast, change, or withdraw — is counted against three windows:
//! per Account/minute, per Account/day, and per hashed client IP/hour. The
//! counters are ordinary [`RateLimit`] rows checked inside the same
//! `TransactWriteItems` as the vote itself, so a refused vote never increments
//! a counter.
//!
//! The trusted-edge IP handling lives in [`crate::http`], shared with every
//! other module that needs the same privacy boundary.

use chrono::{DateTime, Datelike, Timelike, Utc};
use data::model::RateLimitPartition;

/// RATELIMIT action shared by every vote window.
pub const VOTE_ACTION: &str = "vote";
/// Per-Account votes per minute (PRD §8).
pub const ACCOUNT_VOTES_PER_MINUTE: i64 = 30;
/// Per-Account votes per day (PRD §8).
pub const ACCOUNT_VOTES_PER_DAY: i64 = 1_000;
/// Per-hashed-IP votes per hour (PRD §8).
pub const IP_VOTES_PER_HOUR: i64 = 500;

/// Window granularity used by a counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Minute,
    Hour,
    Day,
}

impl WindowKind {
    /// `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH`, or `YYYY-MM-DD`.
    pub fn bucket(self, now: DateTime<Utc>) -> String {
        match self {
            Self::Minute => now.format("%Y-%m-%dT%H:%M").to_string(),
            Self::Hour => now.format("%Y-%m-%dT%H").to_string(),
            Self::Day => now.format("%Y-%m-%d").to_string(),
        }
    }

    /// Epoch seconds at which the current window ends.
    pub fn ends_at(self, now: DateTime<Utc>) -> i64 {
        match self {
            Self::Minute => {
                let start = now
                    .with_second(0)
                    .and_then(|value| value.with_nanosecond(0))
                    .expect("truncating to a minute is always valid");
                start.timestamp() + 60
            }
            Self::Hour => {
                let start = now
                    .with_minute(0)
                    .and_then(|value| value.with_second(0))
                    .and_then(|value| value.with_nanosecond(0))
                    .expect("truncating to an hour is always valid");
                start.timestamp() + 60 * 60
            }
            Self::Day => {
                let start = chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), now.day())
                    .expect("the current date is valid")
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc()
                    .timestamp();
                start + 24 * 60 * 60
            }
        }
    }
}

/// One counter to add and condition in the vote transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitWindow {
    pub partition: RateLimitPartition,
    pub bucket: String,
    pub limit: i64,
    pub expires_at: i64,
    pub retry_after: i64,
}

/// The three vote windows for this request. The IP window is absent when the
/// trusted edge did not supply an address; the Account limits still apply.
pub fn vote_windows(
    account_id: &str,
    ip_hash: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<RateLimitWindow> {
    let mut windows = vec![
        window(
            RateLimitPartition::Account(account_id.to_owned()),
            WindowKind::Minute,
            ACCOUNT_VOTES_PER_MINUTE,
            now,
        ),
        window(
            RateLimitPartition::Account(account_id.to_owned()),
            WindowKind::Day,
            ACCOUNT_VOTES_PER_DAY,
            now,
        ),
    ];

    if let Some(ip_hash) = ip_hash {
        windows.push(window(
            RateLimitPartition::IpHash(ip_hash.to_owned()),
            WindowKind::Hour,
            IP_VOTES_PER_HOUR,
            now,
        ));
    }

    windows
}

pub(crate) fn window(
    partition: RateLimitPartition,
    kind: WindowKind,
    limit: i64,
    now: DateTime<Utc>,
) -> RateLimitWindow {
    let ends_at = kind.ends_at(now);
    RateLimitWindow {
        partition,
        bucket: kind.bucket(now),
        limit,
        expires_at: ends_at,
        retry_after: (ends_at - now.timestamp()).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .expect("test timestamp is valid")
    }

    #[test]
    fn minute_window_bucket_and_retry_after_are_seconds_left() {
        let window = window(
            RateLimitPartition::Account("a".to_owned()),
            WindowKind::Minute,
            ACCOUNT_VOTES_PER_MINUTE,
            at(2026, 11, 8, 12, 30, 15),
        );

        assert_eq!(window.bucket, "2026-11-08T12:30");
        assert_eq!(window.retry_after, 45, "60 - 15 seconds elapsed");
        assert_eq!(
            window.expires_at,
            at(2026, 11, 8, 12, 31, 0).timestamp(),
            "the window ends on the next minute boundary"
        );
    }

    #[test]
    fn hour_window_counts_the_remaining_seconds() {
        let window = window(
            RateLimitPartition::IpHash("h".to_owned()),
            WindowKind::Hour,
            IP_VOTES_PER_HOUR,
            at(2026, 11, 8, 12, 30, 15),
        );

        assert_eq!(window.bucket, "2026-11-08T12");
        assert_eq!(window.retry_after, 1785, "3600 - (30 * 60 + 15)");
    }

    #[test]
    fn day_window_runs_to_utc_midnight() {
        let window = window(
            RateLimitPartition::Account("a".to_owned()),
            WindowKind::Day,
            ACCOUNT_VOTES_PER_DAY,
            at(2026, 11, 8, 12, 30, 15),
        );

        assert_eq!(window.bucket, "2026-11-08");
        assert_eq!(window.retry_after, 41_385, "86400 - (12h + 30m + 15s)");
        assert_eq!(window.expires_at, at(2026, 11, 9, 0, 0, 0).timestamp());
    }

    #[test]
    fn vote_windows_are_two_accounts_plus_one_ip() {
        let now = at(2026, 11, 8, 12, 30, 15);
        let windows = vote_windows("account-1", Some("hash-1"), now);

        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows[0].partition,
            RateLimitPartition::Account("account-1".to_owned())
        );
        assert_eq!(windows[0].limit, ACCOUNT_VOTES_PER_MINUTE);
        assert_eq!(
            windows[1].partition,
            RateLimitPartition::Account("account-1".to_owned())
        );
        assert_eq!(windows[1].limit, ACCOUNT_VOTES_PER_DAY);
        assert_eq!(
            windows[2].partition,
            RateLimitPartition::IpHash("hash-1".to_owned())
        );
        assert_eq!(windows[2].limit, IP_VOTES_PER_HOUR);
    }

    #[test]
    fn missing_edge_ip_leaves_only_account_windows() {
        let windows = vote_windows("account-1", None, at(2026, 11, 8, 12, 30, 15));

        assert_eq!(windows.len(), 2);
        assert!(
            windows
                .iter()
                .all(|window| matches!(window.partition, RateLimitPartition::Account(_)))
        );
    }
}
