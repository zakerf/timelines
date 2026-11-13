//! Fixed-window report limits (PRD §8).
//!
//! Reporting is counted against two daily windows: 20 per Account and 100 per
//! hashed client IP. The counters live in the same `TransactWriteItems` as the
//! REPORT item, so a refused report never consumes a slot and never writes a
//! report (PRD §8).
//!
//! The primitives come from the vote module's rate-limit machinery: the window
//! bucket/TTL math and the trusted-edge IP handling are identical, only the
//! action and limits differ.

use chrono::{DateTime, Utc};
use data::model::RateLimitPartition;

use crate::vote::ratelimit::{RateLimitWindow, WindowKind, window};

/// RATELIMIT action shared by both report windows.
pub const REPORT_ACTION: &str = "report";
/// Per-Account reports per day (PRD §8).
pub const ACCOUNT_REPORTS_PER_DAY: i64 = 20;
/// Per-hashed-IP reports per day (PRD §8).
pub const IP_REPORTS_PER_DAY: i64 = 100;

/// The two daily report windows for this request. The IP window is absent when
/// the trusted edge did not supply an address; the Account limit still applies.
pub fn report_windows(
    account_id: &str,
    ip_hash: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<RateLimitWindow> {
    let mut windows = vec![window(
        RateLimitPartition::Account(account_id.to_owned()),
        WindowKind::Day,
        ACCOUNT_REPORTS_PER_DAY,
        now,
    )];

    if let Some(ip_hash) = ip_hash {
        windows.push(window(
            RateLimitPartition::IpHash(ip_hash.to_owned()),
            WindowKind::Day,
            IP_REPORTS_PER_DAY,
            now,
        ));
    }

    windows
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
    fn report_windows_are_one_account_and_one_ip_per_day() {
        let now = at(2026, 11, 8, 12, 30, 15);
        let windows = report_windows("account-1", Some("hash-1"), now);

        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows[0].partition,
            RateLimitPartition::Account("account-1".to_owned())
        );
        assert_eq!(windows[0].limit, ACCOUNT_REPORTS_PER_DAY);
        assert_eq!(windows[0].bucket, "2026-11-08");
        assert_eq!(windows[0].retry_after, 41_385, "86400 - (12h + 30m + 15s)");
        assert_eq!(
            windows[1].partition,
            RateLimitPartition::IpHash("hash-1".to_owned())
        );
        assert_eq!(windows[1].limit, IP_REPORTS_PER_DAY);
        assert_eq!(windows[1].bucket, "2026-11-08");
    }

    #[test]
    fn missing_edge_ip_leaves_only_the_account_window() {
        let windows = report_windows("account-1", None, at(2026, 11, 8, 12, 30, 15));

        assert_eq!(windows.len(), 1);
        assert_eq!(
            windows[0].partition,
            RateLimitPartition::Account("account-1".to_owned())
        );
    }
}
