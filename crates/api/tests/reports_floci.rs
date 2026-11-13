//! The full report slice against floci: idempotent REPORT items, transactional
//! `reportCount`/`flagged` transitions, withdrawal, anonymity, and both
//! rate-limit partitions.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test reports_floci -- --ignored
//! ```
//!
//! Ignored by default so `cargo test --workspace` stays green without floci.

#![cfg(feature = "floci")]

use std::error::Error;

use api::auth::store;
use api::config::ApiConfig;
use api::{AppState, app};
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use data::item::DynamoItem;
use data::model::{
    Account, EditPayload, Emphasis, Event, RateLimit, RateLimitPartition, Report, ReportTargetType,
    Session, Suggestion, SuggestionKind, SuggestionPayload, Timeline,
};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const TIMELINE_ID: &str = "reports-it-timeline";
const EVENT_ID: &str = "reports-it-event";
const SUGGESTION_ID: &str = "reports-it-suggestion";

const OWNER: &str = "reports-it-owner";
const REPORTER_A: &str = "reports-it-a";
const REPORTER_B: &str = "reports-it-b";
const REPORTER_C: &str = "reports-it-c";
const LIMITED: &str = "reports-it-limited";
const IP_LIMITED: &str = "reports-it-ip-limited";
const OTHER_IP: &str = "reports-it-other-ip";

const OWNER_SESSION: &str = "reports-it-owner-session";
const REPORTER_A_SESSION: &str = "reports-it-a-session";
const REPORTER_B_SESSION: &str = "reports-it-b-session";
const REPORTER_C_SESSION: &str = "reports-it-c-session";
const LIMITED_SESSION: &str = "reports-it-limited-session";
const IP_LIMITED_SESSION: &str = "reports-it-ip-limited-session";
const OTHER_IP_SESSION: &str = "reports-it-other-ip-session";

/// Independent SHA-256 vectors: sha256("203.0.113.5dev-salt") and
/// sha256("198.51.100.9dev-salt").
const CLOUDFRONT_IP_HASH: &str = "fd016bb9247e8e4b8887e03980495cc3370c029b685e2717164a88d53d1546a8";
const OTHER_IP_HASH: &str = "741bdebb61deefca442a18107c66d0ffe0c0517eda1d61601b93be5777f32645";

const IP_SALT: &str = "dev-salt";

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn reporting_is_idempotent_flagged_at_three_and_anonymous() -> TestResult {
    let ddb = fixture::client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-reports-{suffix}", fixture::DEFAULT_TABLE_NAME);

    fixture::create_table(&ddb, &table).await?;
    let outcome = run(&ddb, &table).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    outcome
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}

fn config(table: &str) -> ApiConfig {
    ApiConfig::from_lookup(|name| match name {
        "DDB_TABLE" => Some(table.to_owned()),
        "IP_SALT" => Some(IP_SALT.to_owned()),
        _ => None,
    })
    .expect("test config parses")
}

fn timeline() -> Timeline {
    let now = Timestamp::now();
    Timeline {
        id: TIMELINE_ID.to_owned(),
        title: "Report Integration Timeline".to_owned(),
        description: "Reports move counts transactionally.".to_owned(),
        owner_id: OWNER.to_owned(),
        collection_id: None,
        event_count: 1,
        upvotes: 0,
        downvotes: 0,
        created_at: now.clone(),
        updated_at: now,
        report_count: 0,
        flagged: false,
        cover_image: None,
    }
}

fn event() -> Event {
    Event {
        timeline_id: TIMELINE_ID.to_owned(),
        event_id: EVENT_ID.to_owned(),
        timestamp: Timestamp::parse("2026-02-04T00:00:00.000Z").expect("test timestamp"),
        date: "2026-02-04".to_owned(),
        title: "Report Integration Event".to_owned(),
        description: "One dated fact.".to_owned(),
        sources: vec!["https://example.org/source".to_owned()],
        emphasis: Emphasis::Standard,
        upvotes: 0,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        created_at: Timestamp::now(),
        created_by: OWNER.to_owned(),
        last_edited_by: None,
        last_edit_suggestion_id: None,
    }
}

fn suggestion() -> Suggestion {
    Suggestion {
        suggestion_id: SUGGESTION_ID.to_owned(),
        timeline_id: TIMELINE_ID.to_owned(),
        owner_id: Some(OWNER.to_owned()),
        state: data::model::SuggestionState::Pending,
        kind: SuggestionKind::Edit,
        target_event_id: Some(EVENT_ID.to_owned()),
        payload: SuggestionPayload::Edit(EditPayload {
            title: Some("A better title".to_owned()),
            ..EditPayload::default()
        }),
        submitted_by: REPORTER_A.to_owned(),
        submitted_at: Timestamp::now(),
        reviewed_by: None,
        reviewed_at: None,
        review_note: None,
        report_count: 0,
        flagged: false,
    }
}

async fn seed_rows(ddb: &Client, table: &str) -> TestResult {
    for item in [
        timeline().to_item(),
        event().to_item(),
        suggestion().to_item(),
    ] {
        ddb.put_item()
            .table_name(table)
            .set_item(Some(item))
            .send()
            .await?;
    }
    Ok(())
}

async fn sign_in(ddb: &Client, table: &str, account_id: &str, session_id: &str) -> TestResult {
    let now = Timestamp::now();
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: account_id.to_owned(),
            display_name: account_id.to_owned(),
            created_at: now.clone(),
            last_seen_at: now,
            refresh_token_ciphertext: None,
        },
    )
    .await
    .map_err(|error| format!("account upsert failed: {error}"))?;
    store::create_session(
        ddb,
        table,
        &Session {
            id: session_id.to_owned(),
            account_id: account_id.to_owned(),
            expires_at: data::timestamp::now_epoch_seconds() + 3600,
        },
    )
    .await
    .map_err(|error| format!("session create failed: {error}"))?;
    Ok(())
}

fn report_request(
    method: Method,
    uri: &str,
    session_id: &str,
    cloudfront_ip: Option<&str>,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("timeline_session={session_id}"));

    if let Some(ip) = cloudfront_ip {
        builder = builder.header("cloudfront-viewer-address", ip);
    }

    Ok(builder.body(Body::empty())?)
}

fn timeline_report_uri() -> String {
    format!("/api/timeline/{TIMELINE_ID}/report")
}

fn event_report_uri() -> String {
    format!("/api/timeline/{TIMELINE_ID}/event/{EVENT_ID}/report")
}

fn suggestion_report_uri() -> String {
    format!("/api/timeline/{TIMELINE_ID}/suggestions/{SUGGESTION_ID}/report")
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

async fn stored_timeline(ddb: &Client, table: &str) -> Timeline {
    let item = get_item(
        ddb,
        table,
        keys::timeline_pk(TIMELINE_ID),
        keys::TIMELINE_SK.to_owned(),
    )
    .await
    .expect("timeline exists");
    Timeline::from_item(item).expect("decode timeline")
}

async fn stored_event(ddb: &Client, table: &str) -> Event {
    let event = event();
    let item = get_item(ddb, table, event.pk(), event.sk())
        .await
        .expect("event exists");
    Event::from_item(item).expect("decode event")
}

async fn stored_suggestion(ddb: &Client, table: &str) -> Suggestion {
    let item = get_item(
        ddb,
        table,
        keys::suggestion_pk(TIMELINE_ID),
        keys::suggestion_sk(SUGGESTION_ID),
    )
    .await
    .expect("suggestion exists");
    Suggestion::from_item(item).expect("decode suggestion")
}

async fn get_item(
    ddb: &Client,
    table: &str,
    pk: String,
    sk: String,
) -> Option<data::item::AttrMap> {
    ddb.get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .consistent_read(true)
        .send()
        .await
        .expect("get item")
        .item()
        .cloned()
}

async fn stored_report(
    ddb: &Client,
    table: &str,
    target_type: ReportTargetType,
    target_id: &str,
    account_id: &str,
) -> Option<Report> {
    let item = get_item(
        ddb,
        table,
        keys::report_pk(target_type, target_id),
        keys::report_sk(account_id),
    )
    .await?;
    Some(Report::from_item(item).expect("decode report"))
}

async fn rate_limit_count(
    ddb: &Client,
    table: &str,
    partition: RateLimitPartition,
    bucket: &str,
) -> i64 {
    let item = get_item(
        ddb,
        table,
        partition.pk(),
        keys::ratelimit_sk("report", bucket),
    )
    .await
    .expect("rate limit exists");
    RateLimit::from_item(item).expect("decode rate limit").count
}

async fn put_rate_limit(
    ddb: &Client,
    table: &str,
    partition: RateLimitPartition,
    bucket: &str,
    count: i64,
) -> TestResult {
    let now = data::timestamp::now_epoch_seconds();
    let item = RateLimit {
        partition,
        action: "report".to_owned(),
        window_bucket: bucket.to_owned(),
        count,
        expires_at: now + 26 * 60 * 60,
    };
    ddb.put_item()
        .table_name(table)
        .set_item(Some(item.to_item()))
        .send()
        .await?;
    Ok(())
}

fn day_bucket(offset_days: i64) -> String {
    let now = Utc::now() + Duration::days(offset_days);
    now.format("%Y-%m-%d").to_string()
}

async fn run(ddb: &Client, table: &str) -> TestResult {
    seed_rows(ddb, table).await?;
    sign_in(ddb, table, OWNER, OWNER_SESSION).await?;
    sign_in(ddb, table, REPORTER_A, REPORTER_A_SESSION).await?;
    sign_in(ddb, table, REPORTER_B, REPORTER_B_SESSION).await?;
    sign_in(ddb, table, REPORTER_C, REPORTER_C_SESSION).await?;
    sign_in(ddb, table, LIMITED, LIMITED_SESSION).await?;
    sign_in(ddb, table, IP_LIMITED, IP_LIMITED_SESSION).await?;
    sign_in(ddb, table, OTHER_IP, OTHER_IP_SESSION).await?;

    let app = app(AppState::with_aws(
        config(table),
        None,
        Some(ddb.clone()),
        None,
    ));

    // 1. The first report creates the REPORT item and increments the target.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &timeline_report_uri(),
            REPORTER_A_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = body_json(response).await;
    println!("timeline report A:  {body}");
    assert_eq!(body["targetType"], "TIMELINE");
    assert_eq!(body["targetId"], TIMELINE_ID);
    assert_eq!(body["reportCount"], 1);
    assert_eq!(body["flagged"], false);
    assert_eq!(body["reported"], true);

    let stored = stored_timeline(ddb, table).await;
    assert_eq!((stored.report_count, stored.flagged), (1, false));
    let report = stored_report(
        ddb,
        table,
        ReportTargetType::Timeline,
        TIMELINE_ID,
        REPORTER_A,
    )
    .await
    .expect("REPORT item exists");
    assert_eq!(report.target_type, ReportTargetType::Timeline);
    assert_eq!(report.target_id, TIMELINE_ID);
    assert_eq!(report.account_id, REPORTER_A);
    println!(
        "REPORT key:          {} / {}",
        keys::report_pk(report.target_type, &report.target_id),
        keys::report_sk(&report.account_id)
    );
    assert_eq!(
        rate_limit_count(
            ddb,
            table,
            RateLimitPartition::Account(REPORTER_A.to_owned()),
            &day_bucket(0)
        )
        .await,
        1,
        "a successful report consumes one account-day slot"
    );

    // 2. Re-reporting the same target is idempotent: no new count, no new slot.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &timeline_report_uri(),
            REPORTER_A_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["reportCount"], 1);
    assert_eq!(body["reported"], true);
    assert_eq!(stored_timeline(ddb, table).await.report_count, 1);
    assert_eq!(
        rate_limit_count(
            ddb,
            table,
            RateLimitPartition::Account(REPORTER_A.to_owned()),
            &day_bucket(0)
        )
        .await,
        1,
        "the idempotent re-report consumes no slot"
    );

    // 3. A second distinct account increments again; still below the flag.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &timeline_report_uri(),
            REPORTER_B_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(body_json(response).await["reportCount"], 2);
    let stored = stored_timeline(ddb, table).await;
    assert_eq!((stored.report_count, stored.flagged), (2, false));

    // 4. The third distinct account flags the target.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &timeline_report_uri(),
            REPORTER_C_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    println!("timeline report C:  {body}");
    assert_eq!(body["reportCount"], 3);
    assert_eq!(body["flagged"], true);
    let stored = stored_timeline(ddb, table).await;
    assert_eq!((stored.report_count, stored.flagged), (3, true));

    // 5. The owner sees counts and flags only, never reporter identities.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::GET,
            &format!("/api/timeline/{TIMELINE_ID}/reports"),
            OWNER_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let summary = body_text(response).await;
    println!("owner summary:      {summary}");
    let summary_json: serde_json::Value = serde_json::from_str(&summary)?;
    assert_eq!(summary_json["timeline"]["reportCount"], 3);
    assert_eq!(summary_json["timeline"]["flagged"], true);
    for reporter in [REPORTER_A, REPORTER_B, REPORTER_C] {
        assert!(
            !summary.contains(reporter),
            "the owner must never see reporter {reporter}: {summary}"
        );
    }
    assert!(!summary.contains("ACCOUNT#"), "{summary}");

    // The stored target carries no reporter identity either.
    let raw = get_item(
        ddb,
        table,
        keys::timeline_pk(TIMELINE_ID),
        keys::TIMELINE_SK.to_owned(),
    )
    .await
    .expect("timeline exists");
    for attribute in ["accountId", "reportedBy", "reporters"] {
        assert!(
            !raw.contains_key(attribute),
            "the target gained reporter identity in {attribute}"
        );
    }

    // A non-owner cannot read the summary.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::GET,
            &format!("/api/timeline/{TIMELINE_ID}/reports"),
            REPORTER_A_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 6. Withdrawal removes the REPORT item and decrements; below three unflags.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::DELETE,
            &timeline_report_uri(),
            REPORTER_A_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    println!("withdraw A:         {body}");
    assert_eq!(body["reportCount"], 2);
    assert_eq!(body["flagged"], false);
    assert_eq!(body["reported"], false);
    assert!(
        stored_report(
            ddb,
            table,
            ReportTargetType::Timeline,
            TIMELINE_ID,
            REPORTER_A
        )
        .await
        .is_none()
    );
    assert_eq!(
        stored_timeline(ddb, table).await.report_count,
        2,
        "B and C still report"
    );

    // Withdrawing again is a no-op.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::DELETE,
            &timeline_report_uri(),
            REPORTER_A_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["reportCount"], 2);
    assert_eq!(stored_timeline(ddb, table).await.report_count, 2);

    // 7. After a withdrawal the same account can report again.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &timeline_report_uri(),
            REPORTER_A_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["reportCount"], 3);
    assert_eq!(body["flagged"], true);

    // 8. Event-level reports take the same path.
    for session in [REPORTER_B_SESSION, REPORTER_C_SESSION, OWNER_SESSION] {
        let response = app
            .clone()
            .oneshot(report_request(
                Method::POST,
                &event_report_uri(),
                session,
                Some("203.0.113.5:443"),
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let stored = stored_event(ddb, table).await;
    println!(
        "event reports:      count={} flagged={}",
        stored.report_count, stored.flagged
    );
    assert_eq!((stored.report_count, stored.flagged), (3, true));

    // 9. Suggestion reports take the same path, including withdrawal.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &suggestion_report_uri(),
            REPORTER_A_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(body_json(response).await["reportCount"], 1);
    let stored = stored_suggestion(ddb, table).await;
    assert_eq!((stored.report_count, stored.flagged), (1, false));

    let response = app
        .clone()
        .oneshot(report_request(
            Method::GET,
            &format!("/api/timeline/{TIMELINE_ID}/reports"),
            OWNER_SESSION,
            None,
        )?)
        .await?;
    let summary = body_json(response).await;
    assert_eq!(summary["events"][EVENT_ID]["reportCount"], 3);
    assert_eq!(summary["events"][EVENT_ID]["flagged"], true);
    assert_eq!(summary["suggestions"][SUGGESTION_ID]["reportCount"], 1);
    assert_eq!(summary["suggestions"][SUGGESTION_ID]["flagged"], false);

    let response = app
        .clone()
        .oneshot(report_request(
            Method::DELETE,
            &suggestion_report_uri(),
            REPORTER_A_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["reportCount"], 0);
    assert_eq!(stored_suggestion(ddb, table).await.report_count, 0);

    // 10. Unknown targets refuse with 404 and write nothing.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            "/api/timeline/reports-it-missing/report",
            REPORTER_A_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // 11. Per-Account daily fixed window: 20/day.
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(LIMITED.to_owned()),
        &day_bucket(0),
        20,
    )
    .await?;
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &event_report_uri(),
            LIMITED_SESSION,
            Some("203.0.113.7:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: i64 = response.headers()[header::RETRY_AFTER]
        .to_str()?
        .parse()
        .expect("Retry-After is whole seconds");
    println!("account day 429:    Retry-After={retry_after}");
    assert!(
        (1..=86_400).contains(&retry_after),
        "expected at most a day, got {retry_after}"
    );
    assert!(
        stored_report(ddb, table, ReportTargetType::Event, EVENT_ID, LIMITED)
            .await
            .is_none(),
        "a refused report writes no REPORT item"
    );

    // 12. A different IP is a different partition.
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &event_report_uri(),
            OTHER_IP_SESSION,
            Some("198.51.100.9:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        rate_limit_count(
            ddb,
            table,
            RateLimitPartition::IpHash(OTHER_IP_HASH.to_owned()),
            &day_bucket(0)
        )
        .await,
        1
    );

    // 13. Per-hashed-IP daily fixed window: 100/day. Both today and tomorrow are
    //     full so an overnight boundary cannot make the test flaky.
    for offset in [0, 1] {
        put_rate_limit(
            ddb,
            table,
            RateLimitPartition::IpHash(CLOUDFRONT_IP_HASH.to_owned()),
            &day_bucket(offset),
            100,
        )
        .await?;
    }
    let response = app
        .clone()
        .oneshot(report_request(
            Method::POST,
            &suggestion_report_uri(),
            IP_LIMITED_SESSION,
            Some("203.0.113.5:443"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: i64 = response.headers()[header::RETRY_AFTER]
        .to_str()?
        .parse()
        .expect("Retry-After is whole seconds");
    println!("ip day 429:         Retry-After={retry_after}");
    assert!(
        (1..=86_400).contains(&retry_after),
        "expected at most a day, got {retry_after}"
    );
    assert!(
        stored_report(
            ddb,
            table,
            ReportTargetType::Suggestion,
            SUGGESTION_ID,
            IP_LIMITED
        )
        .await
        .is_none()
    );

    Ok(())
}
