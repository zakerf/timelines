//! The full Collection browsing slice against floci: CRUD through the API,
//! GSI2 membership rows, and the re-rendered Collection/Home pages in S3.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test collections_floci -- --ignored
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
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use data::item::DynamoItem;
use data::model::{Account, Session, Timeline};
use data::{Timestamp, fixture, keys};
use http_body_util::BodyExt;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const SESSION_ID: &str = "collections-it-session";
const ACCOUNT_ID: &str = "collections-it-owner";
const INTRUDER_SESSION_ID: &str = "collections-it-intruder-session";
const INTRUDER_ID: &str = "collections-it-intruder";

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

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<String>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("timeline_session={cookie}"));
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body)
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response")
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

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn collection_membership_drives_collection_and_home_pages() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-collections-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-collections-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    create_bucket(&s3, &bucket).await?;

    let outcome = run(&ddb, &s3, &table, &bucket).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    delete_bucket(&s3, &bucket).await;
    outcome
}

async fn run(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    sign_in(ddb, table, ACCOUNT_ID, SESSION_ID).await?;
    sign_in(ddb, table, INTRUDER_ID, INTRUDER_SESSION_ID).await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    // 1. Create a root collection and a nested child.
    let response = send(
        &app,
        Method::POST,
        "/api/collections",
        SESSION_ID,
        Some(
            r#"{"title":"Elections 2026","description":"Timelines around the cycle."}"#.to_owned(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let root = body_json(response).await;
    let root_id = root["id"].as_str().expect("root id").to_owned();
    assert!(root_id.starts_with("elections-2026-"));
    assert_eq!(root["title"], "Elections 2026");

    let response = send(
        &app,
        Method::POST,
        "/api/collections",
        SESSION_ID,
        Some(
            serde_json::json!({
                "title": "Senate Races",
                "description": "Nested under the cycle.",
                "parentCollectionId": root_id,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let child = body_json(response).await;
    let child_id = child["id"].as_str().expect("child id").to_owned();
    assert_eq!(child["parentCollectionId"], root_id);

    // 2. A Collection page lists its nested child.
    let root_html = object_text(s3, bucket, &format!("collection/{root_id}.html"))
        .await?
        .expect("root collection page uploaded");
    assert!(root_html.contains("Elections 2026"));
    assert!(root_html.contains("Senate Races"));
    assert!(root_html.contains(&format!("href=\"/collection/{child_id}\"")));

    // 3. Publish a Timeline into the root collection.
    let publish = serde_json::json!({
        "title": "Ohio Senate Integration",
        "description": "A timeline moved between collections by the integration test.",
        "collectionId": root_id,
        "events": [{
            "date": "2026-02-04",
            "title": "Candidate filing deadline",
            "description": "Petitions were due.",
            "sources": ["https://www.ohiosos.gov/elections/candidates/"]
        }]
    });
    let response = send(
        &app,
        Method::POST,
        "/api/timeline",
        SESSION_ID,
        Some(publish.to_string()),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        body_text(response).await
    );
    let timelines = renderer::dynamo::scan_timelines(ddb, table).await?;
    let timeline_id = timelines
        .iter()
        .find(|timeline| timeline.title == "Ohio Senate Integration")
        .expect("the published timeline is in the table")
        .id
        .clone();

    let root_html = object_text(s3, bucket, &format!("collection/{root_id}.html"))
        .await?
        .expect("root page exists");
    assert!(
        root_html.contains("Ohio Senate Integration"),
        "the timeline starts in the root collection"
    );
    let child_html = object_text(s3, bucket, &format!("collection/{child_id}.html"))
        .await?
        .expect("child page exists");
    assert!(!child_html.contains("Ohio Senate Integration"));

    // 4. Move the Timeline to the nested child: both collection pages and Home
    //    re-render, and the GSI2 membership attributes move with it.
    let response = send(
        &app,
        Method::PUT,
        &format!("/api/timeline/{timeline_id}/collection"),
        SESSION_ID,
        Some(serde_json::json!({ "collectionId": child_id }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let membership = body_json(response).await;
    assert_eq!(membership["timelineId"], timeline_id);
    assert_eq!(membership["collectionId"], child_id);

    let root_html = object_text(s3, bucket, &format!("collection/{root_id}.html"))
        .await?
        .expect("root page exists");
    assert!(
        !root_html.contains("Ohio Senate Integration"),
        "the old collection page was re-rendered without the timeline"
    );
    let child_html = object_text(s3, bucket, &format!("collection/{child_id}.html"))
        .await?
        .expect("child page exists");
    assert!(
        child_html.contains("Ohio Senate Integration"),
        "the new collection page lists the timeline"
    );
    assert!(
        child_html.contains("index__group-title"),
        "timelines render in update-year groups"
    );
    let home = object_text(s3, bucket, "index.html")
        .await?
        .expect("home page exists");
    assert!(home.contains("Ohio Senate Integration"));
    assert!(home.contains("Elections 2026"));

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
    let stored_timeline = Timeline::from_item(stored.clone())?;
    assert_eq!(
        stored_timeline.collection_id.as_deref(),
        Some(child_id.as_str())
    );
    assert_eq!(
        stored.get("GSI2PK"),
        Some(&AttributeValue::S(child_id.clone())),
        "the GSI2 partition follows the adjacency pointer"
    );

    // 5. Only the creator may rename it.
    let response = send(
        &app,
        Method::PATCH,
        &format!("/api/collections/{child_id}"),
        INTRUDER_SESSION_ID,
        Some(r#"{"title":"Hijacked"}"#.to_owned()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &app,
        Method::PATCH,
        &format!("/api/collections/{child_id}"),
        SESSION_ID,
        Some(r#"{"title":"Senate Contests"}"#.to_owned()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["title"], "Senate Contests");
    let root_html = object_text(s3, bucket, &format!("collection/{root_id}.html"))
        .await?
        .expect("root page exists");
    assert!(
        root_html.contains("Senate Contests"),
        "the parent's child list shows the new title"
    );
    let child_html = object_text(s3, bucket, &format!("collection/{child_id}.html"))
        .await?
        .expect("child page exists");
    assert!(child_html.contains("Senate Contests"));

    // 6. A non-empty collection cannot be deleted.
    let response = send(
        &app,
        Method::DELETE,
        &format!("/api/collections/{root_id}"),
        SESSION_ID,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // 7. Removing a Timeline from its collection re-renders the page empty and
    //    leaves the Timeline on Home as uncollected.
    let response = send(
        &app,
        Method::PUT,
        &format!("/api/timeline/{timeline_id}/collection"),
        SESSION_ID,
        Some(r#"{"collectionId":null}"#.to_owned()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let child_html = object_text(s3, bucket, &format!("collection/{child_id}.html"))
        .await?
        .expect("child page exists");
    assert!(!child_html.contains("Ohio Senate Integration"));
    assert!(child_html.contains("No Timelines in this collection yet."));
    let home = object_text(s3, bucket, "index.html")
        .await?
        .expect("home page exists");
    assert!(home.contains("Ohio Senate Integration"));

    // 8. An empty collection deletes and its page object disappears.
    let response = send(
        &app,
        Method::DELETE,
        &format!("/api/collections/{child_id}"),
        SESSION_ID,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        object_text(s3, bucket, &format!("collection/{child_id}.html"))
            .await?
            .is_none(),
        "the deleted collection's page is removed from the bucket"
    );
    let root_html = object_text(s3, bucket, &format!("collection/{root_id}.html"))
        .await?
        .expect("root page exists");
    assert!(
        !root_html.contains("Senate Contests"),
        "the parent no longer lists the deleted child"
    );

    Ok(())
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}

/// A urlencoded body, the way the Collection page's and the publish page's
/// forms send one. `accept_html` asks for a page rather than JSON.
async fn form_send(
    app: &Router,
    uri: &str,
    cookie: &str,
    body: &str,
    accept_html: bool,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::COOKIE, format!("timeline_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if accept_html {
        builder = builder.header(header::ACCEPT, "text/html,application/xhtml+xml");
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_owned())).expect("request"))
        .await
        .expect("response")
}

async fn create_collection(app: &Router, title: &str) -> Fallible<String> {
    let response = send(
        app,
        Method::POST,
        "/api/collections",
        SESSION_ID,
        Some(serde_json::json!({ "title": title }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    Ok(body_json(response).await["id"]
        .as_str()
        .expect("collection id")
        .to_owned())
}

type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn collection_forms_and_the_publish_picker_drive_the_pages() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-collection-forms-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let bucket = format!("timeline-collection-forms-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    create_bucket(&s3, &bucket).await?;

    let outcome = run_forms(&ddb, &s3, &table, &bucket).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    delete_bucket(&s3, &bucket).await;
    outcome
}

async fn run_forms(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    bucket: &str,
) -> TestResult {
    sign_in(ddb, table, ACCOUNT_ID, SESSION_ID).await?;

    let state = AppState::with_aws(config(table, bucket), None, Some(ddb.clone()), None)
        .with_storage(Some(s3.clone()), None);
    let app = app(state);

    // 1. Two Collections to pick between.
    let alpha = create_collection(&app, "Alpha Cycle").await?;
    let beta = create_collection(&app, "Beta Cycle").await?;

    // 2. Publishing through the form's picker joins the chosen Collection; no
    //    id is typed anywhere.
    let publish_form = format!(
        "title=Picker+Timeline&description=Picked+from+the+list\
         &collectionId={alpha}\
         &events=2026-02-04+%7C+Filing+deadline+%7C+https%3A%2F%2Fexample.org%2Fa+%7C+ok"
    );
    let response = form_send(&app, "/api/timeline", SESSION_ID, &publish_form, false).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()[header::LOCATION]
            .to_str()?
            .starts_with("/timeline/")
    );

    let timelines = renderer::dynamo::scan_timelines(ddb, table).await?;
    let picked = timelines
        .iter()
        .find(|timeline| timeline.title == "Picker Timeline")
        .expect("the form publish created the Timeline");
    assert_eq!(picked.collection_id.as_deref(), Some(alpha.as_str()));
    let alpha_html = object_text(s3, bucket, &format!("collection/{alpha}.html"))
        .await?
        .expect("the picked collection page exists");
    assert!(alpha_html.contains("Picker Timeline"));

    // 3. Inline create: the picker's "__new" value plus a title makes a
    //    Collection and publishes into it.
    let inline_form = format!(
        "title=Inline+Timeline&description=Made+with+its+collection\
         &collectionId=__new&newCollectionTitle=Inline+Created\
         &events=2026-03-01+%7C+Second+fact+%7C+https%3A%2F%2Fexample.org%2Fb+%7C+ok"
    );
    let response = form_send(&app, "/api/timeline", SESSION_ID, &inline_form, false).await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "{}",
        body_text(response).await
    );

    let collections = renderer::dynamo::scan_collections(ddb, table).await?;
    let inline = collections
        .iter()
        .find(|collection| collection.title == "Inline Created")
        .expect("the inline Collection row exists");
    let timelines = renderer::dynamo::scan_timelines(ddb, table).await?;
    let inline_timeline = timelines
        .iter()
        .find(|timeline| timeline.title == "Inline Timeline")
        .expect("the inline Timeline exists");
    assert_eq!(
        inline_timeline.collection_id.as_deref(),
        Some(inline.id.as_str()),
        "the new Timeline joins the new Collection"
    );
    let inline_html = object_text(s3, bucket, &format!("collection/{}.html", inline.id))
        .await?
        .expect("the inline collection page is rendered by the publish");
    assert!(inline_html.contains("Inline Timeline"));

    // 4. A referenced Collection with no row is refused and mints no page.
    let ghost = serde_json::json!({
        "title": "Ghost Collection Timeline",
        "description": "Names a collection that does not exist.",
        "collectionId": "ghost-collection",
        "events": [{
            "date": "2026-04-01",
            "title": "A fact",
            "description": "It happened.",
            "sources": ["https://example.org/c"]
        }]
    });
    let response = send(
        &app,
        Method::POST,
        "/api/timeline",
        SESSION_ID,
        Some(ghost.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let refusal = body_json(response).await;
    assert_eq!(refusal["errors"][0]["field"], "collectionId", "{refusal}");
    assert!(
        renderer::dynamo::scan_timelines(ddb, table)
            .await?
            .iter()
            .all(|timeline| timeline.title != "Ghost Collection Timeline"),
        "a refused publish writes no Timeline"
    );
    assert!(
        object_text(s3, bucket, "collection/ghost-collection.html")
            .await?
            .is_none(),
        "a dangling id never becomes a live page"
    );

    // 5. Rename and re-describe through the page's own form.
    let response = form_send(
        &app,
        &format!("/api/collections/{alpha}"),
        SESSION_ID,
        "action=rename&title=Renamed+Cycle&description=New+deck&parentCollectionId=",
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION].to_str()?,
        format!("/collection/{alpha}")
    );
    let alpha_html = object_text(s3, bucket, &format!("collection/{alpha}.html"))
        .await?
        .expect("the renamed page exists");
    assert!(alpha_html.contains("Renamed Cycle"));
    assert!(alpha_html.contains("New deck"));
    assert!(!alpha_html.contains("Alpha Cycle"), "{alpha_html}");

    // 6. Nest it under the other Collection through the picker; both pages
    //    reflect the hierarchy.
    let response = form_send(
        &app,
        &format!("/api/collections/{alpha}"),
        SESSION_ID,
        &format!("action=rename&title=Renamed+Cycle&parentCollectionId={beta}"),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let alpha_html = object_text(s3, bucket, &format!("collection/{alpha}.html"))
        .await?
        .expect("the nested page exists");
    assert!(
        alpha_html.contains("part of")
            && alpha_html.contains(&format!("href=\"/collection/{beta}\"")),
        "the page says what it is part of: {alpha_html}"
    );
    let beta_html = object_text(s3, bucket, &format!("collection/{beta}.html"))
        .await?
        .expect("the parent page exists");
    assert!(
        beta_html.contains(&format!("href=\"/collection/{alpha}\"")),
        "the parent lists the nested Collection"
    );

    // 7. Add and remove a Timeline through the membership form.
    let response = form_send(
        &app,
        &format!("/api/collections/{beta}/membership"),
        SESSION_ID,
        &format!("timelineId={}", picked.id),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let beta_html = object_text(s3, bucket, &format!("collection/{beta}.html"))
        .await?
        .expect("the parent page exists");
    assert!(beta_html.contains("Picker Timeline"));
    let alpha_html = object_text(s3, bucket, &format!("collection/{alpha}.html"))
        .await?
        .expect("the old page exists");
    assert!(
        !alpha_html.contains("Picker Timeline"),
        "the Timeline left the first Collection"
    );

    let response = form_send(
        &app,
        &format!("/api/collections/{beta}/membership"),
        SESSION_ID,
        &format!("action=remove&timelineId={}", picked.id),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let beta_html = object_text(s3, bucket, &format!("collection/{beta}.html"))
        .await?
        .expect("the parent page exists");
    assert!(!beta_html.contains("Picker Timeline"));

    // 8. A non-empty Collection refuses deletion with an explanation, as a
    //    page, and nothing disappears.
    let response = form_send(
        &app,
        &format!("/api/collections/{beta}"),
        SESSION_ID,
        "action=delete",
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8",
        "a browser gets an explanation, not a one-line body"
    );
    let refusal = body_text(response).await;
    assert!(refusal.contains("not empty"), "{refusal}");
    assert!(refusal.contains("Move or remove"), "{refusal}");
    assert!(
        object_text(s3, bucket, &format!("collection/{beta}.html"))
            .await?
            .is_some(),
        "a refused delete changes nothing"
    );

    // 9. An empty Collection deletes and its page object disappears.
    let response = form_send(
        &app,
        &format!("/api/collections/{alpha}"),
        SESSION_ID,
        "action=delete",
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION].to_str()?, "/");
    assert!(
        object_text(s3, bucket, &format!("collection/{alpha}.html"))
            .await?
            .is_none(),
        "the empty Collection's page is removed"
    );

    Ok(())
}
