//! The owner settings surface against floci: metadata save, cover removal,
//! unpublish/publish, delete, and authorization.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test manage_floci -- --ignored
//! ```

#![cfg(feature = "floci")]

use std::error::Error;

use api::auth::store;
use api::config::ApiConfig;
use api::{AppState, app};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use data::item::DynamoItem;
use data::model::{Account, CoverImage, Event, Session, Significance, Timeline};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const OWNER: &str = "manage-it-owner";
const OWNER_SESSION: &str = "manage-it-owner-session";
const INTRUDER: &str = "manage-it-intruder";
const INTRUDER_SESSION: &str = "manage-it-intruder-session";
const TIMELINE_ID: &str = "manage-it-timeline";

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
    serde_json::from_str(&body_text(response).await).expect("json body")
}

async fn sign_in(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    id: &str,
    session: &str,
) -> TestResult {
    let now = Timestamp::now();
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: id.to_owned(),
            display_name: format!("{id} display"),
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
            id: session.to_owned(),
            account_id: id.to_owned(),
            expires_at: data::timestamp::now_epoch_seconds() + 3600,
        },
    )
    .await
    .map_err(|error| format!("session create failed: {error}"))?;
    Ok(())
}

async fn get(app: &Router, uri: &str, session: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(session) = session {
        builder = builder.header(header::COOKIE, format!("timeline_session={session}"));
    }
    builder = builder.header(header::ACCEPT, "text/html,application/xhtml+xml");
    app.clone()
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

async fn post(app: &Router, uri: &str, session: &str, body: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::COOKIE, format!("timeline_session={session}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ACCEPT, "text/html,application/xhtml+xml")
                .body(Body::from(body.to_owned()))
                .expect("request"),
        )
        .await
        .expect("response")
}

fn timeline(published: bool) -> Timeline {
    Timeline {
        id: TIMELINE_ID.to_owned(),
        title: "Manage It Timeline".to_owned(),
        description: "A timeline the settings tests manage.".to_owned(),
        owner_id: OWNER.to_owned(),
        collection_id: None,
        event_count: 1,
        upvotes: 0,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        published,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
        cover_image: Some(CoverImage {
            key: format!("cover/{TIMELINE_ID}/hero.jpg"),
            width: 1600,
            height: 900,
            credit: Some("Photo: Test".to_owned()),
        }),
    }
}

fn event() -> Event {
    Event {
        timeline_id: TIMELINE_ID.to_owned(),
        event_id: "manage-it-event".to_owned(),
        timestamp: Timestamp::from_date("2026-02-04").expect("date"),
        date: "2026-02-04".to_owned(),
        title: "Candidate filing deadline".to_owned(),
        description: "Petitions were due.".to_owned(),
        sources: vec!["https://example.org/a".to_owned()],
        significance: Significance::Regular,
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

async fn seed(ddb: &aws_sdk_dynamodb::Client, table: &str, published: bool) -> TestResult {
    for row in [timeline(published).to_item(), event().to_item()] {
        ddb.put_item()
            .table_name(table)
            .set_item(Some(row))
            .send()
            .await?;
    }
    Ok(())
}

async fn stored_timeline(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<Option<Timeline>, Box<dyn Error + Send + Sync>> {
    let output = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?;
    Ok(output
        .item()
        .cloned()
        .map(Timeline::from_item)
        .transpose()?)
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn owner_settings_manage_the_whole_publication() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-manage-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-manage-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    create_bucket(&s3, &bucket).await?;

    let outcome = run(&ddb, &s3, &table, &bucket).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    let _ = s3.delete_bucket().bucket(&bucket).send().await;
    outcome
}

async fn run(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    sign_in(ddb, table, OWNER, OWNER_SESSION).await?;
    sign_in(ddb, table, INTRUDER, INTRUDER_SESSION).await?;
    seed(ddb, table, true).await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    let settings_uri = format!("/api/timeline/{TIMELINE_ID}/settings");

    // 1. Anonymous is sent to sign in; a non-owner is refused with a page.
    let response = get(&app, &settings_uri, None).await;
    assert!(
        response.status().is_redirection(),
        "anonymous settings redirects to sign-in"
    );

    let response = get(&app, &settings_uri, Some(INTRUDER_SESSION)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let refusal = body_text(response).await;
    assert!(
        refusal.contains("Only the Timeline&#39;s owner"),
        "{refusal}"
    );

    let response = get(&app, &settings_uri, Some(OWNER_SESSION)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let settings = body_text(response).await;
    assert!(settings.contains("Timeline settings"), "{settings}");
    assert!(settings.contains("value=\"Manage It Timeline\""));
    assert!(settings.contains("src=\"/media/cover/manage-it-timeline/hero.jpg\""));
    assert!(settings.contains("Photo: Test"));

    // 2. Save the details: the row, the Timeline page, and Home all update.
    //    (The page exists after the save's re-render.)
    let response = post(
        &app,
        &settings_uri,
        OWNER_SESSION,
        "action=save&title=A+Renamed+Timeline&description=A+new+description",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION].to_str()?,
        format!("{settings_uri}?saved=details")
    );
    let stored = stored_timeline(ddb, table).await?.expect("timeline exists");
    assert_eq!(stored.title, "A Renamed Timeline");
    assert_eq!(stored.description, "A new description");
    let page = object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("the Timeline page was rendered");
    assert!(page.contains("A Renamed Timeline"));
    let home = object_text(s3, bucket, "index.html").await?.expect("home");
    assert!(home.contains("A Renamed Timeline"));

    // 3. A blank title is refused on the page and changes nothing.
    let response = post(
        &app,
        &settings_uri,
        OWNER_SESSION,
        "action=save&title=+&description=No+title",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let refusal = body_text(response).await;
    assert!(refusal.contains("A Timeline needs a title."), "{refusal}");
    assert_eq!(
        stored_timeline(ddb, table).await?.expect("timeline").title,
        "A Renamed Timeline"
    );

    // 4. Move it into a Collection from the same form.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/collections")
                .header(header::COOKIE, format!("timeline_session={OWNER_SESSION}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"Settings Cycle"}"#.to_owned()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let collection_id = body_json(response).await["id"]
        .as_str()
        .expect("collection id")
        .to_owned();

    let response = post(
        &app,
        &settings_uri,
        OWNER_SESSION,
        &format!(
            "action=save&title=A+Renamed+Timeline&description=A+new+description&collectionId={collection_id}"
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let stored = stored_timeline(ddb, table).await?.expect("timeline");
    assert_eq!(
        stored.collection_id.as_deref(),
        Some(collection_id.as_str())
    );
    let collection_html = object_text(s3, bucket, &format!("collection/{collection_id}.html"))
        .await?
        .expect("the collection page was rendered");
    assert!(collection_html.contains("A Renamed Timeline"));

    // 5. Removing the cover clears the attribute and the page's hero.
    let response = post(&app, &settings_uri, OWNER_SESSION, "action=remove-cover").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        stored_timeline(ddb, table)
            .await?
            .expect("timeline")
            .cover_image
            .is_none(),
        "the cover attribute is gone"
    );
    let page = object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("the Timeline page was rendered");
    assert!(
        !page.contains("<img class=\"hero__cover\""),
        "the hero fell back: {page}"
    );

    // 6. Unpublish: the page and its list entries disappear, the row stays.
    let response = post(&app, &settings_uri, OWNER_SESSION, "action=unpublish").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        stored_timeline(ddb, table)
            .await?
            .expect("timeline")
            .published,
        false
    );
    assert!(
        object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
            .await?
            .is_none(),
        "the page object is deleted"
    );
    let home = object_text(s3, bucket, "index.html").await?.expect("home");
    assert!(!home.contains("A Renamed Timeline"), "Home drops it");
    let settings = body_text(get(&app, &settings_uri, Some(OWNER_SESSION)).await).await;
    assert!(
        settings.contains("This Timeline is unpublished"),
        "{settings}"
    );
    assert!(settings.contains("value=\"publish\""));

    // 7. Publishing again brings the page and the lists back.
    let response = post(&app, &settings_uri, OWNER_SESSION, "action=publish").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
            .await?
            .is_some(),
        "the page object is rendered again"
    );
    let home = object_text(s3, bucket, "index.html").await?.expect("home");
    assert!(home.contains("A Renamed Timeline"));

    // 8. A non-owner cannot invoke any action.
    let response = post(&app, &settings_uri, INTRUDER_SESSION, "action=delete").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(stored_timeline(ddb, table).await?.is_some());

    // 9. Delete removes the row, the Events, and the page.
    let response = post(&app, &settings_uri, OWNER_SESSION, "action=delete").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION].to_str()?, "/");
    assert!(
        stored_timeline(ddb, table).await?.is_none(),
        "the Timeline row is gone"
    );
    let events = ddb
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk")
        .expression_attribute_values(":pk", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .send()
        .await?;
    assert!(
        events.items().is_empty(),
        "the Timeline partition (Events) is gone"
    );
    assert!(
        object_text(s3, bucket, &format!("timeline/{TIMELINE_ID}.html"))
            .await?
            .is_none(),
        "the page object is gone"
    );
    let home = object_text(s3, bucket, "index.html").await?.expect("home");
    assert!(!home.contains("A Renamed Timeline"));
    let collection_html = object_text(s3, bucket, &format!("collection/{collection_id}.html"))
        .await?
        .expect("the collection page exists");
    assert!(!collection_html.contains("A Renamed Timeline"));

    Ok(())
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}
