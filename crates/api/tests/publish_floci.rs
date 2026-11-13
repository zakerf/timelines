//! The full publish slice against floci: checklist refusal, the DynamoDB
//! transaction, the in-process render, and the S3 `PUT`.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test publish_floci -- --ignored
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
use data::model::{Account, Session, Timeline};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const SESSION_ID: &str = "publish-it-session";
const ACCOUNT_ID: &str = "publish-it-owner";

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
        // floci serves path-style bucket URLs.
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
async fn publishing_writes_rows_renders_pages_and_returns_a_fragment() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-publish-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-publish-{suffix}");

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
    // A signed-in owner: the ACCOUNT row and an unexpired SESSION row.
    let now = Timestamp::now();
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: ACCOUNT_ID.to_owned(),
            display_name: "Publish Owner".to_owned(),
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

    // 1. Checklist refusal: 400 and a per-field error list.
    let response = app
        .clone()
        .oneshot(json_request(r#"{"title":"","events":[]}"#)?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["error"], "checklist_failed");
    let fields: Vec<&str> = json["errors"]
        .as_array()
        .expect("errors array")
        .iter()
        .map(|error| error["field"].as_str().expect("field name"))
        .collect();
    assert_eq!(fields, ["title", "events"]);

    // 2. A valid publish writes the rows, renders the page set, and returns the
    //    HTMX fragment.
    let publish = serde_json::json!({
        "title": "Publish Integration Timeline",
        "description": "A timeline published by the integration test.",
        "collectionId": "elections-2026",
        "events": [
            {
                "date": "2026-02-04",
                "title": "Candidate filing deadline",
                "description": "Petitions were due.",
                "sources": ["https://www.ohiosos.gov/elections/candidates/"]
            },
            {
                "date": "2026-05-05",
                "title": "Primary election day",
                "description": "Nominees were decided.",
                "sources": ["https://example.org/primary"]
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(json_request(&publish.to_string())?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let fragment = body_text(response).await;
    assert!(fragment.contains("data-timeline-id="), "{fragment}");
    assert!(fragment.contains("Publish Integration Timeline"));
    assert!(fragment.contains("Candidate filing deadline"));
    assert!(fragment.contains("Primary election day"));
    assert!(fragment.contains("2 event(s)"));

    let timeline_id = attribute(&fragment, "data-timeline-id");

    // 3. The TIMELINE row and its two EVENT rows landed with the owner stamped,
    //    eventCount bumped, and UTC timestamps.
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
    assert_eq!(timeline.title, "Publish Integration Timeline");
    assert_eq!(timeline.collection_id.as_deref(), Some("elections-2026"));
    assert_eq!(
        timeline.event_count, 2,
        "initial eventCount is the event total"
    );
    assert_eq!(timeline.upvotes, 0);
    assert_eq!(timeline.downvotes, 0);
    assert_eq!(timeline.created_at, timeline.updated_at);
    Timestamp::parse(timeline.created_at.as_str())?;

    let events = renderer::dynamo::load_events(ddb, table, &timeline_id).await?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].date, "2026-02-04");
    assert_eq!(events[0].title, "Candidate filing deadline");
    assert_eq!(events[0].created_by, ACCOUNT_ID);
    assert_eq!(
        events[0].timestamp.as_str(),
        "2026-02-04T00:00:00.000Z",
        "date-only events get midnight UTC"
    );

    // 4. The affected page set is in the site bucket, rendered by the API
    //    in-process. Home and the collection page are part of the set too.
    let timeline_key = format!("timeline/{timeline_id}.html");
    let page = object_text(s3, bucket, &timeline_key)
        .await?
        .expect("timeline page was uploaded");
    assert!(page.contains("Publish Integration Timeline"));
    assert!(page.contains("Candidate filing deadline"));
    assert!(page.contains("Primary election day"));
    assert!(page.contains("data-up=\"0\""), "baked vote counts");
    assert!(page.contains("htmx.org"), "the read page ships HTMX");
    assert!(
        !page.contains("__RESOLVED_RESOURCES"),
        "no Leptos hydration bookkeeping"
    );

    let home = object_text(s3, bucket, "index.html")
        .await?
        .expect("home page was uploaded");
    assert!(home.contains("Publish Integration Timeline"));
    assert!(home.contains(&format!("/timeline/{timeline_id}")));

    let collection = object_text(s3, bucket, "collection/elections-2026.html")
        .await?
        .expect("collection page was uploaded");
    assert!(collection.contains("Publish Integration Timeline"));

    // 5. The rendered page is served from floci S3 at the object URL.
    let served = object_text(s3, bucket, &timeline_key)
        .await?
        .expect("published page is served");
    assert_eq!(served, page);

    Ok(())
}

fn json_request(body: &str) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri("/api/timeline")
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
