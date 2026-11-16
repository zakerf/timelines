//! The JSON import slice against floci: per-field refusal, the single
//! transaction (TIMELINE + N EVENTs + the publish-limit counters), the
//! in-process render, the S3 `PUT`, and the `429` refusals.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test import_floci -- --ignored
//! ```
//!
//! Ignored by default so `cargo test --workspace` stays green without floci.

#![cfg(feature = "floci")]

use std::error::Error;

use api::auth::store;
use api::config::ApiConfig;
use api::publish::{EVENT_LIMIT_ACTION, TIMELINE_LIMIT_ACTION};
use api::{AppState, app};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use data::item::DynamoItem;
use data::model::{Account, RateLimit, RateLimitPartition, Session, Significance, Timeline};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const SESSION_ID: &str = "import-it-session";
const ACCOUNT_ID: &str = "import-it-owner";

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

async fn create_bucket(s3: &aws_sdk_s3::Client, bucket: &str) -> TestResult {
    s3.create_bucket()
        .bucket(bucket)
        .create_bucket_configuration(
            CreateBucketConfiguration::builder()
                .location_constraint(BucketLocationConstraint::EuCentral1)
                .build(),
        )
        .send()
        .await?;
    Ok(())
}

async fn delete_bucket(s3: &aws_sdk_s3::Client, bucket: &str) {
    let _ = s3.delete_bucket().bucket(bucket).send().await;
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

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
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

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn importing_writes_rows_renders_pages_and_rides_the_publish_limits() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-import-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-import-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    create_bucket(&s3, &bucket).await?;

    let outcome = run(&ddb, &s3, &table, &bucket).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    delete_bucket(&s3, &bucket).await;
    outcome
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}

async fn run(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    let now = Timestamp::now();
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: ACCOUNT_ID.to_owned(),
            display_name: "Import Owner".to_owned(),
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
            id: SESSION_ID.to_owned(),
            account_id: ACCOUNT_ID.to_owned(),
            expires_at: data::timestamp::now_epoch_seconds() + 3600,
        },
    )
    .await
    .map_err(|error| format!("session create failed: {error}"))?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    // 1. A malformed document is a 400 with per-field errors and writes nothing.
    let response = app
        .clone()
        .oneshot(import_request(r#"{"title":"T","events":[]}"#)?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["error"], "invalid_request");
    assert_eq!(json["errors"][0]["field"], "description");

    // 2. A valid import: out-of-order Events land earliest-first, the rows and
    //    the limit counters commit together, and the fragment comes back.
    let document = serde_json::json!({
        "title": "Import Integration Timeline",
        "description": "A timeline imported from the research-timeline document.",
        "events": [
            {
                "date": "2026-05-05",
                "title": "Primary election day",
                "description": "Nominees were decided.",
                "sources": ["https://example.org/primary"]
            },
            {
                "date": "2026-02-04",
                "title": "Candidate filing deadline",
                "description": "Petitions were due.",
                "sources": ["https://www.ohiosos.gov/elections/candidates/"]
            },
            {
                "date": "2026-03-01T09:30:00Z",
                "title": "Certification of candidates",
                "description": "The board certified the field.",
                "sources": ["https://example.org/certification"]
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(import_request(&document.to_string())?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let fragment = body_text(response).await;
    assert!(fragment.contains("data-timeline-id="), "{fragment}");
    assert!(fragment.contains("Import Integration Timeline"));
    assert!(fragment.contains("3 event(s)"));

    let timeline_id = attribute(&fragment, "data-timeline-id");

    // 3. TIMELINE: title, description, eventCount, regular-significance events,
    //    and no collection (an import is uncollected).
    let stored = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(&timeline_id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?
        .item()
        .expect("timeline row exists")
        .clone();
    let timeline = Timeline::from_item(stored)?;
    assert_eq!(timeline.id, timeline_id);
    assert_eq!(timeline.owner_id, ACCOUNT_ID);
    assert_eq!(timeline.title, "Import Integration Timeline");
    assert_eq!(
        timeline.description,
        "A timeline imported from the research-timeline document."
    );
    assert_eq!(timeline.collection_id, None, "an import is uncollected");
    assert_eq!(timeline.event_count, 3, "eventCount is the import size");

    let events = renderer::dynamo::load_events(ddb, table, &timeline_id).await?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].date, "2026-02-04", "earliest event comes first");
    assert_eq!(events[0].title, "Candidate filing deadline");
    assert_eq!(
        events[0].timestamp.as_str(),
        "2026-02-04T00:00:00.000Z",
        "a date-only value gets midnight UTC"
    );
    assert_eq!(events[1].title, "Certification of candidates");
    assert_eq!(
        events[1].timestamp.as_str(),
        "2026-03-01T09:30:00.000Z",
        "a date-time keeps its time"
    );
    assert_eq!(events[2].date, "2026-05-05");
    for event in &events {
        assert_eq!(event.significance, Significance::Regular);
        assert_eq!(event.created_by, ACCOUNT_ID);
    }

    // 4. The limit counters rode the same transaction.
    let timeline_count = counter(ddb, table, TIMELINE_LIMIT_ACTION).await?;
    assert_eq!(timeline_count, 1);
    let event_count = counter(ddb, table, EVENT_LIMIT_ACTION).await?;
    assert_eq!(event_count, 3);

    // 5. Re-render wrote the exact page set: the Timeline page and home.
    let timeline_key = format!("timeline/{timeline_id}.html");
    let page = object_text(s3, bucket, &timeline_key)
        .await?
        .expect("timeline page was uploaded");
    assert!(page.contains("Import Integration Timeline"));
    let filing = page
        .find("Candidate filing deadline")
        .expect("filing deadline rendered");
    let primary = page.find("Primary election day").expect("primary rendered");
    assert!(filing < primary, "the rendered spine is chronological");
    assert!(
        page.contains("/assets/htmx.min.js"),
        "the read page ships the vendored HTMX"
    );
    assert!(
        !page.contains("__RESOLVED_RESOURCES"),
        "no Leptos hydration bookkeeping"
    );

    let home = object_text(s3, bucket, "index.html")
        .await?
        .expect("home page was uploaded");
    assert!(home.contains("Import Integration Timeline"));
    assert!(home.contains(&format!("/timeline/{timeline_id}")));

    // 6. The daily Timeline limit refuses with 429 + Retry-After.
    seed_counter(ddb, table, TIMELINE_LIMIT_ACTION, 10).await?;
    let response = app
        .clone()
        .oneshot(import_request(&one_event_document())?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: u64 = response.headers()[header::RETRY_AFTER]
        .to_str()?
        .parse()
        .expect("Retry-After is decimal seconds");
    assert!((1..=86_400).contains(&retry_after), "{retry_after}");
    assert_eq!(body_json(response).await["error"], "rate_limited");

    // 7. The daily Event limit refuses the same way.
    seed_counter(ddb, table, TIMELINE_LIMIT_ACTION, 0).await?;
    seed_counter(ddb, table, EVENT_LIMIT_ACTION, 100).await?;
    let response = app
        .clone()
        .oneshot(import_request(&one_event_document())?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(response).await["error"], "rate_limited");

    Ok(())
}

fn one_event_document() -> String {
    serde_json::json!({
        "title": "Another Imported Timeline",
        "description": "One dated fact.",
        "events": [{
            "date": "2026-06-01",
            "title": "Filing opens",
            "description": "Filing opened.",
            "sources": ["https://example.org/filing"]
        }]
    })
    .to_string()
}

async fn counter(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    action: &str,
) -> Result<i64, Box<dyn Error + Send + Sync>> {
    let item = ddb
        .get_item()
        .table_name(table)
        .key(
            "PK",
            AttributeValue::S(keys::ratelimit_account_pk(ACCOUNT_ID)),
        )
        .key(
            "SK",
            AttributeValue::S(keys::ratelimit_sk(action, &Timestamp::now().date_bucket())),
        )
        .send()
        .await?
        .item()
        .expect("counter exists")
        .clone();
    Ok(RateLimit::from_item(item)?.count)
}

async fn seed_counter(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    action: &str,
    count: i64,
) -> TestResult {
    let now = Timestamp::now();
    let limit = RateLimit {
        partition: RateLimitPartition::Account(ACCOUNT_ID.to_owned()),
        action: action.to_owned(),
        window_bucket: now.date_bucket(),
        count,
        expires_at: data::timestamp::now_epoch_seconds() + 86_400,
    };
    ddb.put_item()
        .table_name(table)
        .set_item(Some(limit.to_item()))
        .send()
        .await?;
    Ok(())
}

fn import_request(body: &str) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri("/api/timeline/import")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("timeline_session={SESSION_ID}"))
        .body(Body::from(body.to_owned()))?)
}

/// Pull one simple `name="value"` attribute out of the returned fragment.
fn attribute(fragment: &str, name: &str) -> String {
    let needle = format!("{name}=\"");
    let start = fragment.find(&needle).expect("attribute is present") + needle.len();
    let end = fragment[start..].find('"').expect("attribute closes") + start;
    fragment[start..end].to_owned()
}
