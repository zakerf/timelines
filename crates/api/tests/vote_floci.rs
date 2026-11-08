//! The full vote slice against floci: transactional counters, own-vote state,
//! self-vote refusal, and both rate-limit partitions.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test vote_floci -- --ignored
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
use chrono::{Datelike, Utc};
use data::item::DynamoItem;
use data::model::{
    Account, Emphasis, Event, RateLimit, RateLimitPartition, Session, Timeline, Vote, VoteDirection,
};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const TIMELINE_ID: &str = "vote-it-timeline";
const EVENT_ID: &str = "vote-it-event";
const VOTER: &str = "vote-it-voter";
const OWNER: &str = "vote-it-owner";
const LIMITED: &str = "vote-it-limited";
const DAY_LIMITED: &str = "vote-it-day-limited";
const IP_LIMITED: &str = "vote-it-ip-limited";
const OTHER_IP: &str = "vote-it-other-ip";
const SPOOFED: &str = "vote-it-spoofed";

const VOTER_SESSION: &str = "vote-it-voter-session";
const OWNER_SESSION: &str = "vote-it-owner-session";
const LIMITED_SESSION: &str = "vote-it-limited-session";
const DAY_LIMITED_SESSION: &str = "vote-it-day-limited-session";
const IP_LIMITED_SESSION: &str = "vote-it-ip-limited-session";
const OTHER_IP_SESSION: &str = "vote-it-other-ip-session";
const SPOOFED_SESSION: &str = "vote-it-spoofed-session";

/// Independent SHA-256 vectors: sha256("203.0.113.5dev-salt") and
/// sha256("198.51.100.9dev-salt").
const CLOUDFRONT_IP_HASH: &str = "fd016bb9247e8e4b8887e03980495cc3370c029b685e2717164a88d53d1546a8";
const OTHER_IP_HASH: &str = "741bdebb61deefca442a18107c66d0ffe0c0517eda1d61601b93be5777f32645";

const IP_SALT: &str = "dev-salt";

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn voting_updates_counters_enforces_limits_and_returns_own_state() -> TestResult {
    let ddb = fixture::client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-votes-{suffix}", fixture::DEFAULT_TABLE_NAME);

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
        title: "Vote Integration Timeline".to_owned(),
        description: "Counters must move transactionally.".to_owned(),
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
        title: "Vote Integration Event".to_owned(),
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

async fn seed_rows(ddb: &Client, table: &str) -> TestResult {
    ddb.put_item()
        .table_name(table)
        .set_item(Some(timeline().to_item()))
        .send()
        .await?;
    ddb.put_item()
        .table_name(table)
        .set_item(Some(event().to_item()))
        .send()
        .await?;
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

fn vote_request(
    session_id: &str,
    event_id: Option<&str>,
    direction: &str,
    cloudfront_ip: Option<&str>,
    forwarded_for: Option<&str>,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    let uri = match event_id {
        Some(event_id) => format!("/api/timeline/{TIMELINE_ID}/event/{event_id}/vote"),
        None => format!("/api/timeline/{TIMELINE_ID}/vote"),
    };
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("timeline_session={session_id}"));

    if let Some(ip) = cloudfront_ip {
        builder = builder.header("cloudfront-viewer-address", ip);
    }
    if let Some(ip) = forwarded_for {
        builder = builder.header("x-forwarded-for", ip);
    }

    Ok(builder.body(Body::from(format!("direction={direction}")))?)
}

async fn own_votes_request(
    session_id: &str,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    Ok(Request::builder()
        .method(Method::GET)
        .uri(format!("/api/timeline/{TIMELINE_ID}/votes"))
        .header(header::COOKIE, format!("timeline_session={session_id}"))
        .body(Body::empty())?)
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
    let item = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .consistent_read(true)
        .send()
        .await
        .expect("get timeline")
        .item()
        .expect("timeline exists")
        .clone();
    Timeline::from_item(item).expect("decode timeline")
}

async fn stored_event(ddb: &Client, table: &str) -> Event {
    let event = event();
    let item = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(event.pk()))
        .key("SK", AttributeValue::S(event.sk()))
        .consistent_read(true)
        .send()
        .await
        .expect("get event")
        .item()
        .expect("event exists")
        .clone();
    Event::from_item(item).expect("decode event")
}

async fn stored_vote(
    ddb: &Client,
    table: &str,
    account_id: &str,
    event_id: Option<&str>,
) -> Option<Vote> {
    let sk = match event_id {
        Some(event_id) => keys::event_vote_sk(TIMELINE_ID, event_id),
        None => keys::timeline_vote_sk(TIMELINE_ID),
    };
    ddb.get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::vote_pk(account_id)))
        .key("SK", AttributeValue::S(sk))
        .consistent_read(true)
        .send()
        .await
        .expect("get vote")
        .item()
        .map(|item| Vote::from_item(item.clone()).expect("decode vote"))
}

async fn rate_limit_count(
    ddb: &Client,
    table: &str,
    partition: RateLimitPartition,
    bucket: &str,
) -> i64 {
    let item = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(partition.pk()))
        .key("SK", AttributeValue::S(keys::ratelimit_sk("vote", bucket)))
        .consistent_read(true)
        .send()
        .await
        .expect("get rate limit")
        .item()
        .expect("rate limit exists")
        .clone();

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
        action: "vote".to_owned(),
        window_bucket: bucket.to_owned(),
        count,
        expires_at: now + 3_600,
    };
    ddb.put_item()
        .table_name(table)
        .set_item(Some(item.to_item()))
        .send()
        .await?;
    Ok(())
}

fn minute_bucket(offset_minutes: i64) -> String {
    let now = Utc::now() + chrono::Duration::minutes(offset_minutes);
    now.format("%Y-%m-%dT%H:%M").to_string()
}

fn hour_bucket(offset_hours: i64) -> String {
    let now = Utc::now() + chrono::Duration::hours(offset_hours);
    now.format("%Y-%m-%dT%H").to_string()
}

fn today() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

async fn run(ddb: &Client, table: &str) -> TestResult {
    seed_rows(ddb, table).await?;
    sign_in(ddb, table, VOTER, VOTER_SESSION).await?;
    sign_in(ddb, table, OWNER, OWNER_SESSION).await?;
    sign_in(ddb, table, LIMITED, LIMITED_SESSION).await?;
    sign_in(ddb, table, DAY_LIMITED, DAY_LIMITED_SESSION).await?;
    sign_in(ddb, table, IP_LIMITED, IP_LIMITED_SESSION).await?;
    sign_in(ddb, table, OTHER_IP, OTHER_IP_SESSION).await?;
    sign_in(ddb, table, SPOOFED, SPOOFED_SESSION).await?;

    let app = app(AppState::with_aws(
        config(table),
        None,
        Some(ddb.clone()),
        None,
    ));

    let minute = minute_bucket(0);

    // 1. Cast an upvote at Timeline level.
    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let fragment = body_text(response).await;
    assert!(fragment.contains("data-my-vote=\"up\""), "{fragment}");
    assert!(fragment.contains("data-up=\"1\""), "{fragment}");
    assert!(fragment.contains("data-down=\"0\""), "{fragment}");

    let stored = stored_timeline(ddb, table).await;
    println!(
        "timeline cast up:   up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (1, 0));
    assert_eq!(
        stored_vote(ddb, table, VOTER, None)
            .await
            .map(|vote| vote.direction),
        Some(VoteDirection::Up)
    );
    assert_eq!(
        rate_limit_count(
            ddb,
            table,
            RateLimitPartition::Account(VOTER.to_owned()),
            &minute
        )
        .await,
        1,
        "a successful vote consumes one account-minute slot"
    );

    // 2. Change the same vote to a downvote.
    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            None,
            "down",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let fragment = body_text(response).await;
    assert!(fragment.contains("data-my-vote=\"down\""), "{fragment}");
    assert!(fragment.contains("data-up=\"0\""), "{fragment}");
    assert!(fragment.contains("data-down=\"1\""), "{fragment}");

    let stored = stored_timeline(ddb, table).await;
    println!(
        "timeline change down: up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (0, 1));
    assert_eq!(
        stored_vote(ddb, table, VOTER, None)
            .await
            .map(|vote| vote.direction),
        Some(VoteDirection::Down)
    );

    // 3. Withdraw: the downvote disappears and nothing replaces it.
    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            None,
            "none",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let fragment = body_text(response).await;
    assert!(fragment.contains("data-my-vote=\"none\""), "{fragment}");
    assert!(fragment.contains("data-up=\"0\""), "{fragment}");
    assert!(fragment.contains("data-down=\"0\""), "{fragment}");

    let stored = stored_timeline(ddb, table).await;
    println!(
        "timeline withdraw:   up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (0, 0));
    assert!(stored_vote(ddb, table, VOTER, None).await.is_none());

    // 4. Cast again, then read own-vote state in one query.
    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(own_votes_request(VOTER_SESSION).await?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let own = body_json(response).await;
    println!("own votes:          {own}");
    assert_eq!(own["timeline"], "up");
    assert_eq!(own["events"], serde_json::json!({}));

    // 5. Event-level cast/change/withdraw exercises the same transaction.
    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            Some(EVENT_ID),
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("data-my-vote=\"up\""));

    let stored = stored_event(ddb, table).await;
    println!(
        "event cast up:      up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (1, 0));

    let response = app
        .clone()
        .oneshot(own_votes_request(VOTER_SESSION).await?)
        .await?;
    let own = body_json(response).await;
    assert_eq!(own["timeline"], "up");
    assert_eq!(own["events"][EVENT_ID], "up");

    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            Some(EVENT_ID),
            "down",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = stored_event(ddb, table).await;
    println!(
        "event change down:  up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (0, 1));

    let response = app
        .clone()
        .oneshot(vote_request(
            VOTER_SESSION,
            Some(EVENT_ID),
            "none",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = stored_event(ddb, table).await;
    println!(
        "event withdraw:     up={} down={}",
        stored.upvotes, stored.downvotes
    );
    assert_eq!((stored.upvotes, stored.downvotes), (0, 0));
    assert!(
        stored_vote(ddb, table, VOTER, Some(EVENT_ID))
            .await
            .is_none()
    );

    // 6. The Timeline owner cannot vote on their own Timeline or its Events.
    let response = app
        .clone()
        .oneshot(vote_request(
            OWNER_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(vote_request(
            OWNER_SESSION,
            Some(EVENT_ID),
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let stored = stored_timeline(ddb, table).await;
    assert_eq!(
        (stored.upvotes, stored.downvotes),
        (1, 0),
        "self-votes write nothing"
    );
    assert!(stored_vote(ddb, table, OWNER, None).await.is_none());

    // 7. Per-Account fixed window: a full minute counter returns 429 with the
    //    seconds left in the window, and the refused vote writes nothing.
    for offset in [0, 1] {
        put_rate_limit(
            ddb,
            table,
            RateLimitPartition::Account(LIMITED.to_owned()),
            &minute_bucket(offset),
            30,
        )
        .await?;
    }
    let response = app
        .clone()
        .oneshot(vote_request(
            LIMITED_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: i64 = response.headers()[header::RETRY_AFTER]
        .to_str()?
        .parse()
        .expect("Retry-After is whole seconds");
    println!("account minute 429: Retry-After={retry_after}");
    assert!(
        (1..=60).contains(&retry_after),
        "expected at most a minute, got {retry_after}"
    );
    assert!(stored_vote(ddb, table, LIMITED, None).await.is_none());

    // A full day counter is the second Account window.
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(DAY_LIMITED.to_owned()),
        &today(),
        1_000,
    )
    .await?;

    let response = app
        .clone()
        .oneshot(vote_request(
            DAY_LIMITED_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
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
    assert!(stored_vote(ddb, table, DAY_LIMITED, None).await.is_none());

    // 8. Per-hashed-IP fixed window. Both the current and next hour are full so
    //    the test cannot race an hour boundary.
    for offset in [0, 1] {
        put_rate_limit(
            ddb,
            table,
            RateLimitPartition::IpHash(CLOUDFRONT_IP_HASH.to_owned()),
            &hour_bucket(offset),
            500,
        )
        .await?;
    }

    let response = app
        .clone()
        .oneshot(vote_request(
            IP_LIMITED_SESSION,
            None,
            "up",
            Some("203.0.113.5:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: i64 = response.headers()[header::RETRY_AFTER]
        .to_str()?
        .parse()
        .expect("Retry-After is whole seconds");
    println!("ip hour 429:        Retry-After={retry_after}");
    assert!(
        (1..=3_600).contains(&retry_after),
        "expected at most an hour, got {retry_after}"
    );
    assert!(stored_vote(ddb, table, IP_LIMITED, None).await.is_none());

    // A different CloudFront address is a different partition.
    let response = app
        .clone()
        .oneshot(vote_request(
            OTHER_IP_SESSION,
            None,
            "up",
            Some("198.51.100.9:443"),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        stored_vote(ddb, table, OTHER_IP, None)
            .await
            .map(|vote| vote.direction),
        Some(VoteDirection::Up)
    );
    assert_eq!(
        rate_limit_count(
            ddb,
            table,
            RateLimitPartition::IpHash(OTHER_IP_HASH.to_owned()),
            &hour_bucket(0)
        )
        .await,
        1
    );

    // A client-supplied X-Forwarded-For is not the trusted edge address, so it
    // cannot move the request into a fresh IP partition.
    let response = app
        .clone()
        .oneshot(vote_request(
            SPOOFED_SESSION,
            None,
            "up",
            None,
            Some("203.0.113.5"),
        )?)
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "X-Forwarded-For must not be used as the rate-limit IP"
    );

    Ok(())
}
