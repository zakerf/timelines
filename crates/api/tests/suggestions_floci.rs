//! The full Suggestions slice against floci: submit APPEND/EDIT, the owner
//! queue and public list, approve/reject with a note, the Event writes and
//! `eventCount`, the in-process re-render, and the abuse-prevention counters.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test suggestions_floci -- --ignored
//! ```
//!
//! Ignored by default so `cargo test --workspace` stays green without floci.

#![cfg(feature = "floci")]

use std::error::Error;

use api::auth::store;
use api::config::ApiConfig;
use api::{AppState, app};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use data::item::DynamoItem;
use data::model::{
    Account, Emphasis, Event, RateLimit, RateLimitPartition, Session, SuggestCount, Suggestion,
    SuggestionState, Timeline,
};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const OWNER: &str = "suggestion-it-owner";
const AUTHOR: &str = "suggestion-it-author";
const OTHER: &str = "suggestion-it-other";
const OWNER_SESSION: &str = "suggestion-it-owner-session";
const AUTHOR_SESSION: &str = "suggestion-it-author-session";
const OTHER_SESSION: &str = "suggestion-it-other-session";
const TIMELINE_ID: &str = "suggestion-it-timeline";
const EVENT_ID: &str = "suggestion-it-event";
const IP: &str = "203.0.113.9";

fn s3_client() -> aws_sdk_s3::Client {
    let region = std::env::var("AWS_DEFAULT_REGION").unwrap_or_else(|_| "eu-central-1".to_owned());
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(Region::new(region))
        .credentials_provider(Credentials::new(
            std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "test".to_owned()),
            std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "test".to_owned()),
            None,
            None,
            "floci",
        ))
        .endpoint_url(fixture::endpoint_url())
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

fn config(table: &str, bucket: &str) -> ApiConfig {
    ApiConfig::from_lookup(|name| match name {
        "DDB_TABLE" => Some(table.to_owned()),
        "SITE_BUCKET" => Some(bucket.to_owned()),
        _ => None,
    })
    .expect("test config parses")
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}

async fn prepare() -> Result<
    (aws_sdk_dynamodb::Client, aws_sdk_s3::Client, String, String),
    Box<dyn Error + Send + Sync>,
> {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-suggest-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-suggest-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    s3.create_bucket()
        .bucket(&bucket)
        .create_bucket_configuration(
            CreateBucketConfiguration::builder()
                .location_constraint(BucketLocationConstraint::EuCentral1)
                .build(),
        )
        .send()
        .await?;
    Ok((ddb, s3, table, bucket))
}

async fn teardown(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) {
    let _ = fixture::delete_table(ddb, table).await;
    let _ = s3.delete_bucket().bucket(bucket).send().await;
}

async fn seed_identities(ddb: &aws_sdk_dynamodb::Client, table: &str) -> TestResult {
    let now = Timestamp::now();
    for (sub, name, session_id) in [
        (OWNER, "Suggestion Owner", OWNER_SESSION),
        (AUTHOR, "Suggestion Author", AUTHOR_SESSION),
        (OTHER, "Some Other Account", OTHER_SESSION),
    ] {
        store::upsert_account(
            ddb,
            table,
            &Account {
                sub: sub.to_owned(),
                display_name: name.to_owned(),
                created_at: now.clone(),
                last_seen_at: now.clone(),
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
                account_id: sub.to_owned(),
                expires_at: data::timestamp::now_epoch_seconds() + 3600,
            },
        )
        .await
        .map_err(|error| format!("session create failed: {error}"))?;
    }
    Ok(())
}

fn sample_event(event_id: &str, title: &str, sources: Vec<String>) -> Event {
    Event {
        timeline_id: TIMELINE_ID.to_owned(),
        event_id: event_id.to_owned(),
        timestamp: Timestamp::from_date("2026-02-04").unwrap(),
        date: "2026-02-04".to_owned(),
        title: title.to_owned(),
        description: "An existing fact.".to_owned(),
        sources,
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

async fn seed_timeline(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    events: &[Event],
) -> TestResult {
    let now = Timestamp::now();
    let timeline = Timeline {
        id: TIMELINE_ID.to_owned(),
        title: "Suggestion Integration Timeline".to_owned(),
        description: "A timeline the suggestion tests edit.".to_owned(),
        owner_id: OWNER.to_owned(),
        collection_id: None,
        event_count: i64::try_from(events.len())?,
        upvotes: 0,
        downvotes: 0,
        created_at: now.clone(),
        updated_at: now,
        report_count: 0,
        flagged: false,
        cover_image: None,
    };
    put(ddb, table, &timeline).await?;
    for event in events {
        put(ddb, table, event).await?;
    }
    Ok(())
}

async fn put<T: DynamoItem>(ddb: &aws_sdk_dynamodb::Client, table: &str, item: &T) -> TestResult {
    ddb.put_item()
        .table_name(table)
        .set_item(Some(item.to_item()))
        .send()
        .await?;
    Ok(())
}

fn request(
    method: Method,
    uri: &str,
    session: &str,
    body: Option<serde_json::Value>,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("timeline_session={session}"))
        .header("cloudfront-viewer-address", IP);
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    Ok(builder.body(body)?)
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

async fn object_text(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
    let output = match s3.get_object().bucket(bucket).key(key).send().await {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };
    let bytes = output.body.collect().await?.into_bytes();
    Ok(Some(String::from_utf8(bytes.to_vec())?))
}

async fn get_suggestion(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    suggestion_id: &str,
) -> Result<Suggestion, Box<dyn Error + Send + Sync>> {
    let output = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::suggestion_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::suggestion_sk(suggestion_id)))
        .send()
        .await?;
    Ok(Suggestion::from_item(
        output.item().expect("suggestion row exists").clone(),
    )?)
}

fn append_body(title: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "APPEND",
        "payload": {
            "date": "2026-03-01",
            "title": title,
            "description": "Filing opened.",
            "sources": ["https://example.org/filing"]
        }
    })
}

fn edit_body(title: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "EDIT",
        "targetEventId": EVENT_ID,
        "payload": { "title": title }
    })
}

fn suggestions_uri() -> String {
    format!("/api/timeline/{TIMELINE_ID}/suggestions")
}

fn review_uri(suggestion_id: &str, action: &str) -> String {
    format!("/api/timeline/{TIMELINE_ID}/suggestions/{suggestion_id}/{action}")
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn submit_approve_and_reject_end_to_end() -> TestResult {
    let (ddb, s3, table, bucket) = prepare().await?;
    let outcome = run_submit_approve_and_reject(&ddb, &s3, &table, &bucket).await;
    teardown(&ddb, &s3, &table, &bucket).await;
    outcome
}

async fn run_submit_approve_and_reject(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    seed_identities(ddb, table).await?;
    seed_timeline(
        ddb,
        table,
        &[sample_event(
            EVENT_ID,
            "Candidate filing deadline",
            vec!["https://example.org/original".to_owned()],
        )],
    )
    .await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    // 1. Submit an APPEND and an EDIT as a signed-in author.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("A new fact")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let append = body_json(response).await;
    assert_eq!(append["kind"], "APPEND");
    assert_eq!(append["state"], "PENDING");
    assert_eq!(append["submittedBy"], AUTHOR);
    let append_id = append["id"].as_str().expect("append id").to_owned();

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(edit_body("An edited title")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let edit = body_json(response).await;
    assert_eq!(edit["kind"], "EDIT");
    assert_eq!(edit["targetEventId"], EVENT_ID);
    let edit_id = edit["id"].as_str().expect("edit id").to_owned();

    // 2. The owner queue (GSI1) and the public pending list both see them.
    let response = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/suggestions/queue",
            OWNER_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let queue = body_json(response).await;
    let queue_ids: Vec<&str> = queue
        .as_array()
        .expect("queue array")
        .iter()
        .map(|item| item["id"].as_str().expect("id"))
        .collect();
    assert_eq!(queue_ids, [edit_id.as_str(), append_id.as_str()]);

    let response = app
        .clone()
        .oneshot(request(Method::GET, &suggestions_uri(), "", None)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let public = body_json(response).await;
    assert_eq!(public.as_array().expect("public array").len(), 2);

    // The pending row carries the sparse GSI1 attributes at submit time.
    let stored = get_suggestion(ddb, table, &append_id).await?;
    assert_eq!(stored.state, SuggestionState::Pending);
    let raw = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::suggestion_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::suggestion_sk(&append_id)))
        .send()
        .await?
        .item()
        .expect("row")
        .clone();
    assert_eq!(
        raw.get("GSI1PK"),
        Some(&AttributeValue::S(keys::suggestion_gsi1_pk(OWNER)))
    );

    // 3. A non-owner cannot review.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&append_id, "approve"),
            AUTHOR_SESSION,
            Some(serde_json::json!({"note": "self-approve"})),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 4. Approve the APPEND: Event + eventCount + re-render, all in one pass.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&append_id, "approve"),
            OWNER_SESSION,
            Some(serde_json::json!({"note": "Looks right"})),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let approved = body_json(response).await;
    assert_eq!(approved["state"], "APPROVED");
    assert_eq!(approved["reviewedBy"], OWNER);
    assert_eq!(approved["reviewNote"], "Looks right");

    let stored = get_suggestion(ddb, table, &append_id).await?;
    assert_eq!(stored.state, SuggestionState::Approved);
    assert_eq!(stored.reviewed_by.as_deref(), Some(OWNER));
    assert_eq!(stored.review_note.as_deref(), Some("Looks right"));

    let timeline_row = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?
        .item()
        .expect("timeline row")
        .clone();
    let timeline = Timeline::from_item(timeline_row)?;
    assert_eq!(timeline.event_count, 2, "approve-append bumps eventCount");

    let events = renderer::dynamo::load_events(ddb, table, TIMELINE_ID).await?;
    assert!(
        events.iter().any(|event| event.title == "A new fact"),
        "the approved Event was written"
    );
    let page = object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("the re-render uploaded the timeline page");
    assert!(
        page.contains("A new fact"),
        "the page shows the approved append"
    );

    // 5. Approve the EDIT: the Event changes in place, attribution is stamped,
    //    and the page re-renders again.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&edit_id, "approve"),
            OWNER_SESSION,
            Some(serde_json::json!({"note": "Better wording"})),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let approved = body_json(response).await;
    assert_eq!(approved["state"], "APPROVED");

    let events = renderer::dynamo::load_events(ddb, table, TIMELINE_ID).await?;
    let edited = events
        .iter()
        .find(|event| event.event_id == EVENT_ID)
        .expect("the edited Event still exists");
    assert_eq!(edited.title, "An edited title");
    assert_eq!(
        edited.last_edited_by.as_deref(),
        Some(AUTHOR),
        "the edit is attributed to the Suggestion's author"
    );
    assert_eq!(
        edited.last_edit_suggestion_id.as_deref(),
        Some(edit_id.as_str())
    );
    assert_eq!(edited.upvotes, 0, "an edit does not reset counters");

    let page = object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("page");
    assert!(page.contains("An edited title"), "the page shows the edit");

    // 6. The queue and public list are empty once nothing is pending.
    let response = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/suggestions/queue",
            OWNER_SESSION,
            None,
        )?)
        .await?;
    assert_eq!(body_json(response).await.as_array().unwrap().len(), 0);
    let response = app
        .clone()
        .oneshot(request(Method::GET, &suggestions_uri(), "", None)?)
        .await?;
    assert_eq!(body_json(response).await.as_array().unwrap().len(), 0);

    // 7. Rejection requires a note and leaves the content alone.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("A rejected fact")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let rejected = body_json(response).await;
    let rejected_id = rejected["id"].as_str().expect("id").to_owned();

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&rejected_id, "reject"),
            OWNER_SESSION,
            Some(serde_json::json!({})),
        )?)
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "reject-with-note only"
    );

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&rejected_id, "reject"),
            OWNER_SESSION,
            Some(serde_json::json!({"note": "Out of scope"})),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = get_suggestion(ddb, table, &rejected_id).await?;
    assert_eq!(stored.state, SuggestionState::Rejected);
    assert_eq!(stored.review_note.as_deref(), Some("Out of scope"));

    let timeline = Timeline::from_item(
        ddb.get_item()
            .table_name(table)
            .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
            .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
            .send()
            .await?
            .item()
            .expect("timeline")
            .clone(),
    )?;
    assert_eq!(timeline.event_count, 2, "a rejection appends no Event");
    assert!(
        !renderer::dynamo::load_events(ddb, table, TIMELINE_ID)
            .await?
            .iter()
            .any(|event| event.title == "A rejected fact")
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn approving_an_edit_reruns_the_publish_checklist() -> TestResult {
    let (ddb, s3, table, bucket) = prepare().await?;
    let outcome = run_checklist_back_door(&ddb, &s3, &table, &bucket).await;
    teardown(&ddb, &s3, &table, &bucket).await;
    outcome
}

async fn run_checklist_back_door(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    seed_identities(ddb, table).await?;
    // The Timeline already contains one malformed Event (no Source), which the
    // publish checklist must still catch when an unrelated EDIT is approved.
    let malformed = sample_event("suggestion-it-bad", "Unsourced", Vec::new());
    seed_timeline(
        ddb,
        table,
        &[
            sample_event(
                EVENT_ID,
                "Candidate filing deadline",
                vec!["https://example.org/original".to_owned()],
            ),
            malformed,
        ],
    )
    .await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(edit_body("An edited title")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let suggestion_id = body_json(response).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &review_uri(&suggestion_id, "approve"),
            OWNER_SESSION,
            Some(serde_json::json!({"note": "Of course"})),
        )?)
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "the checklist refuses the edit"
    );
    let body = body_json(response).await;
    assert_eq!(body["error"], "checklist_failed");

    let stored = get_suggestion(ddb, table, &suggestion_id).await?;
    assert_eq!(
        stored.state,
        SuggestionState::Pending,
        "a refused approval leaves the Suggestion pending"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn submission_rate_limits_and_pending_cap_are_enforced() -> TestResult {
    let (ddb, s3, table, bucket) = prepare().await?;
    let outcome = run_limits(&ddb, &s3, &table, &bucket).await;
    teardown(&ddb, &s3, &table, &bucket).await;
    outcome
}

async fn run_limits(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    seed_identities(ddb, table).await?;
    seed_timeline(
        ddb,
        table,
        &[sample_event(
            EVENT_ID,
            "Candidate filing deadline",
            vec!["https://example.org/original".to_owned()],
        )],
    )
    .await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    let now = Timestamp::now();
    let hour = now.hour_bucket();
    let day = now.date_bucket();

    // Five submissions fit the 5/hour account window.
    for index in 0..5 {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                &suggestions_uri(),
                AUTHOR_SESSION,
                Some(append_body(&format!("Fact {index}"))),
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::CREATED, "submission {index}");
    }

    // The sixth is refused by the hourly limit, with Retry-After.
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("Fact 6")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    let body = body_json(response).await;
    assert_eq!(body["reason"], "account_hourly");

    let limits = rate_limit_row(ddb, table, &keys::ratelimit_account_pk(AUTHOR), &hour).await?;
    assert_eq!(
        limits.count, 5,
        "a refused write never increments the counter"
    );

    // The per-Timeline pending cap: 10 pending means the next one is refused
    // even though the account counters were reset.
    put(
        ddb,
        table,
        &SuggestCount {
            timeline_id: TIMELINE_ID.to_owned(),
            account_id: AUTHOR.to_owned(),
            pending: 10,
        },
    )
    .await?;
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(AUTHOR.to_owned()),
        &hour,
        0,
    )
    .await?;
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("Over the cap")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(response).await["reason"], "pending_cap");

    // The daily limit, reached independently of the hour window.
    put(
        ddb,
        table,
        &SuggestCount {
            timeline_id: TIMELINE_ID.to_owned(),
            account_id: AUTHOR.to_owned(),
            pending: 0,
        },
    )
    .await?;
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(AUTHOR.to_owned()),
        &hour,
        0,
    )
    .await?;
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(AUTHOR.to_owned()),
        &day,
        20,
    )
    .await?;
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("Over the day")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(response).await["reason"], "account_daily");

    // The hashed-IP partition is written and enforces its own hourly limit.
    let ip_partition = ip_rate_limit_pk(ddb, table).await?;
    put(
        ddb,
        table,
        &SuggestCount {
            timeline_id: TIMELINE_ID.to_owned(),
            account_id: AUTHOR.to_owned(),
            pending: 0,
        },
    )
    .await?;
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(AUTHOR.to_owned()),
        &hour,
        0,
    )
    .await?;
    put_rate_limit(
        ddb,
        table,
        RateLimitPartition::Account(AUTHOR.to_owned()),
        &day,
        0,
    )
    .await?;
    put_rate_limit(ddb, table, ip_partition, &hour, 20).await?;
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &suggestions_uri(),
            AUTHOR_SESSION,
            Some(append_body("Over the IP")),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(response).await["reason"], "ip_hourly");

    Ok(())
}

async fn put_rate_limit(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    partition: RateLimitPartition,
    bucket: &str,
    count: i64,
) -> TestResult {
    put(
        ddb,
        table,
        &RateLimit {
            partition,
            action: "suggest".to_owned(),
            window_bucket: bucket.to_owned(),
            count,
            expires_at: data::timestamp::now_epoch_seconds() + 3600,
        },
    )
    .await
}

async fn rate_limit_row(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    pk: &str,
    hour: &str,
) -> Result<RateLimit, Box<dyn Error + Send + Sync>> {
    let output = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(pk.to_owned()))
        .key("SK", AttributeValue::S(keys::ratelimit_sk("suggest", hour)))
        .send()
        .await?;
    Ok(RateLimit::from_item(
        output.item().expect("rate limit row exists").clone(),
    )?)
}

/// Find the hashed IP partition the API created by scanning for `RL#IP#`.
async fn ip_rate_limit_pk(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<RateLimitPartition, Box<dyn Error + Send + Sync>> {
    let output = ddb
        .scan()
        .table_name(table)
        .filter_expression("begins_with(PK, :prefix)")
        .expression_attribute_values(":prefix", AttributeValue::S("RL#IP#".to_owned()))
        .send()
        .await?;
    let item = output
        .items()
        .first()
        .expect("the IP rate-limit partition exists");
    let limit = RateLimit::from_item(item.clone())?;
    assert!(
        item.get("PK")
            .and_then(|value| value.as_s().ok())
            .is_some_and(|pk| !pk.contains(IP)),
        "only the hash is stored, never the address"
    );
    Ok(limit.partition)
}
