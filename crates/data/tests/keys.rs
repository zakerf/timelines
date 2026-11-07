//! Key builders must produce the exact PK/SK strings from PRD §6.

use data::Timestamp;
use data::keys;
use data::model::ReportTargetType;

#[test]
fn account_keys() {
    assert_eq!(keys::account_pk("abc-123"), "ACCOUNT#abc-123");
    assert_eq!(keys::ACCOUNT_SK, "PROFILE");
}

#[test]
fn collection_keys_and_gsi2() {
    assert_eq!(keys::collection_pk("c1"), "COLLECTION#c1");
    assert_eq!(keys::COLLECTION_SK, "META");
    assert_eq!(keys::collection_gsi2_pk(None), "ROOT");
    assert_eq!(keys::collection_gsi2_pk(Some("parent")), "parent");
    assert_eq!(keys::collection_gsi2_sk("c1"), "C#c1");
}

#[test]
fn timeline_keys_and_gsi2() {
    let created = Timestamp::parse("2026-11-07T12:30:00Z").unwrap();

    assert_eq!(keys::timeline_pk("t1"), "TIMELINE#t1");
    assert_eq!(keys::TIMELINE_SK, "META");
    assert_eq!(keys::timeline_gsi2_pk(None), "ROOT");
    assert_eq!(keys::timeline_gsi2_pk(Some("collection-1")), "collection-1");
    assert_eq!(
        keys::timeline_gsi2_sk(&created, "t1"),
        "T#2026-11-07T12:30:00.000Z#t1"
    );
}

#[test]
fn event_keys_share_the_timeline_partition() {
    let at = Timestamp::parse("2026-11-07T12:30:00Z").unwrap();

    assert_eq!(keys::event_pk("t1"), "TIMELINE#t1");
    assert_eq!(
        keys::event_sk(&at, "e1"),
        "EVENT#2026-11-07T12:30:00.000Z#e1"
    );
    assert_eq!(keys::EVENT_SK_PREFIX, "EVENT#");
}

#[test]
fn vote_keys_for_both_levels() {
    assert_eq!(keys::vote_pk("a1"), "VOTE#a1");
    assert_eq!(keys::timeline_vote_sk("t1"), "T#t1");
    assert_eq!(keys::event_vote_sk("t1", "e1"), "T#t1#EVENT#e1");
    // Own-vote reads use this prefix; it also matches the timeline-level SK.
    assert_eq!(keys::vote_timeline_prefix("t1"), "T#t1");
}

#[test]
fn suggestion_keys_and_owner_queue_gsi1() {
    assert_eq!(keys::suggestion_pk("t1"), "TIMELINE#t1");
    assert_eq!(keys::suggestion_sk("s1"), "SUGGESTION#s1");
    assert_eq!(keys::SUGGESTION_SK_PREFIX, "SUGGESTION#");
    assert_eq!(keys::suggestion_gsi1_pk("owner-1"), "OWNER#owner-1#PENDING");
}

#[test]
fn report_keys() {
    assert_eq!(
        keys::report_pk(ReportTargetType::Timeline, "t1"),
        "REPORT#TIMELINE#t1"
    );
    assert_eq!(
        keys::report_pk(ReportTargetType::Event, "e1"),
        "REPORT#EVENT#e1"
    );
    assert_eq!(
        keys::report_pk(ReportTargetType::Suggestion, "s1"),
        "REPORT#SUGGESTION#s1"
    );
    assert_eq!(keys::report_sk("a1"), "ACCOUNT#a1");
}

#[test]
fn ratelimit_keys() {
    assert_eq!(keys::ratelimit_account_pk("a1"), "RL#ACCOUNT#a1");
    assert_eq!(keys::ratelimit_ip_pk("deadbeef"), "RL#IP#deadbeef");
    assert_eq!(
        keys::ratelimit_sk("vote", "2026-11-07T12:30"),
        "vote#2026-11-07T12:30"
    );
}

#[test]
fn suggestcount_keys() {
    assert_eq!(keys::suggestcount_pk("t1"), "TIMELINE#t1");
    assert_eq!(keys::suggestcount_sk("a1"), "SUGGESTCOUNT#a1");
}

#[test]
fn session_keys() {
    assert_eq!(keys::session_pk("sess-1"), "SESSION#sess-1");
    assert_eq!(keys::SESSION_SK, "META");
}

#[test]
fn timestamps_are_fixed_width_utc_iso8601() {
    let parsed = Timestamp::parse("2026-11-07T13:30:00+01:00").unwrap();
    assert_eq!(parsed.as_str(), "2026-11-07T12:30:00.000Z");

    let earlier = Timestamp::parse("2026-11-07T12:30:00.001Z").unwrap();
    let later = Timestamp::parse("2026-11-07T12:30:00.010Z").unwrap();
    // Lexicographic order matches chronological order.
    assert!(earlier < later);

    let midnight = Timestamp::from_date("2026-11-07").unwrap();
    assert_eq!(midnight.as_str(), "2026-11-07T00:00:00.000Z");
}

#[test]
fn timestamp_window_buckets() {
    let at = Timestamp::parse("2026-11-07T12:34:56Z").unwrap();
    assert_eq!(at.minute_bucket(), "2026-11-07T12:34");
    assert_eq!(at.date_bucket(), "2026-11-07");
}
