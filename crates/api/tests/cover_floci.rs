//! The cover slice against floci: multipart upload -> media object -> the
//! `coverImage` attribute -> re-rendered hero, then replace and remove.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test cover_floci -- --ignored
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

const SESSION_ID: &str = "cover-it-session";
const ACCOUNT_ID: &str = "cover-it-owner";
const STRANGER_SESSION_ID: &str = "cover-it-stranger-session";
const STRANGER_ID: &str = "cover-it-stranger";
const TIMELINE_ID: &str = "cover-it-timeline";
const BOUNDARY: &str = "----timelinecover";

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

fn config(table: &str, site: &str, media: &str) -> ApiConfig {
    ApiConfig::from_lookup(|name| match name {
        "DDB_TABLE" => Some(table.to_owned()),
        "SITE_BUCKET" => Some(site.to_owned()),
        "MEDIA_BUCKET" => Some(media.to_owned()),
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

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
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

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}

/// Minimal PNG header with a readable IHDR (no pixels; the API only parses
/// the header, exactly like a real upload path).
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// Header-valid PNG padded to a target size. The API parses the header and
/// stores the bytes verbatim, so this exercises the body limit without a
/// megabyte of real pixel data.
fn padded_png(width: u32, height: u32, total_len: usize) -> Vec<u8> {
    let mut bytes = png(width, height);
    bytes.resize(total_len, 0);
    bytes
}

fn multipart_request(
    timeline_id: &str,
    session: &str,
    file: &[u8],
    credit: Option<&str>,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"cover.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(file);
    body.extend_from_slice(b"\r\n");

    if let Some(credit) = credit {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"credit\"\r\n\r\n");
        body.extend_from_slice(credit.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    Ok(Request::builder()
        .method(Method::POST)
        .uri(format!("/api/timeline/{timeline_id}/cover"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header(header::COOKIE, format!("timeline_session={session}"))
        .body(Body::from(body))?)
}

fn remove_request(
    timeline_id: &str,
    session: &str,
) -> Result<Request<Body>, Box<dyn Error + Send + Sync>> {
    Ok(Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/timeline/{timeline_id}/cover"))
        .header(header::COOKIE, format!("timeline_session={session}"))
        .body(Body::empty())?)
}

fn attribute(html: &str, name: &str) -> String {
    let needle = format!("{name}=\"");
    let start = html.find(&needle).expect("attribute is present") + needle.len();
    let end = html[start..].find('"').expect("attribute closes") + start;
    html[start..end].to_owned()
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn upload_replace_and_remove_a_cover() -> TestResult {
    let ddb = fixture::client();
    let s3 = s3_client();
    let suffix = format!("{}-{}", std::process::id(), unix_nanos());
    let table = format!("{}-cover-{suffix}", fixture::DEFAULT_TABLE_NAME);
    let site = format!("timeline-cover-site-{suffix}");
    let media = format!("timeline-cover-media-{suffix}");

    fixture::create_table(&ddb, &table).await?;
    create_bucket(&s3, &site).await?;
    create_bucket(&s3, &media).await?;

    let outcome = run(&ddb, &s3, &table, &site, &media).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    delete_bucket(&s3, &site).await;
    delete_bucket(&s3, &media).await;
    outcome
}

async fn run(
    ddb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    table: &str,
    site_bucket: &str,
    media_bucket: &str,
) -> TestResult {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    seed_owner(ddb, table).await?;
    seed_timeline(ddb, table).await?;

    let state = AppState::with_aws(
        config(table, site_bucket, media_bucket),
        None,
        Some(ddb.clone()),
        None,
    )
    .with_storage(Some(s3.clone()), None);
    let app = app(state);

    // 1. A stranger cannot change the cover.
    let response = app
        .clone()
        .oneshot(multipart_request(
            TIMELINE_ID,
            STRANGER_SESSION_ID,
            &png(1600, 900),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 2. Over 4 MiB is refused, but the route accepts bodies above axum's
    //    default 2 MB limit (the per-route DefaultBodyLimit is 4 MiB + slack).
    let response = app
        .clone()
        .oneshot(multipart_request(
            TIMELINE_ID,
            SESSION_ID,
            &padded_png(10, 10, 4 * 1024 * 1024 + 1),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // 3. The owner uploads a 1600x900 PNG over 2 MB with a credit.
    let response = app
        .clone()
        .oneshot(multipart_request(
            TIMELINE_ID,
            SESSION_ID,
            &padded_png(1600, 900, 2_500_000),
            Some("Photo: Demo Author"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let hero = body_text(response).await;
    assert!(
        hero.starts_with("<header ") && hero.contains("class=\"hero hero--cover\""),
        "{hero}"
    );
    assert!(hero.contains("<img "), "{hero}");
    assert!(hero.contains("class=\"hero__cover\""), "{hero}");
    assert!(hero.contains("Photo: Demo Author"), "{hero}");
    assert_eq!(attribute(&hero, "width"), "1600");
    assert_eq!(attribute(&hero, "height"), "900");

    let key = media_src(&hero);
    assert!(
        key.starts_with(&format!("cover/{TIMELINE_ID}/")),
        "server-chosen immutable key: {key}"
    );
    assert!(key.ends_with(".png"), "{key}");

    // 3. The object landed with a pinned content type and immutable caching.
    let head = s3
        .head_object()
        .bucket(media_bucket)
        .key(&key)
        .send()
        .await?;
    assert_eq!(head.content_type(), Some("image/png"));
    assert_eq!(
        head.cache_control(),
        Some("public, max-age=31536000, immutable")
    );

    // 4. The attribute carries the parsed dimensions and credit.
    let cover = stored_cover(ddb, table).await?;
    assert_eq!(cover["key"].as_s().expect("key"), &key);
    assert_eq!(cover["width"].as_n().expect("width"), "1600");
    assert_eq!(cover["height"].as_n().expect("height"), "900");
    assert_eq!(
        cover["credit"].as_s().expect("credit"),
        "Photo: Demo Author"
    );

    // 5. The re-rendered page set points at /media/<key>.
    let page = object_text(s3, site_bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("timeline page was re-rendered");
    assert!(page.contains(&format!("src=\"/media/{key}\"")), "{page}");
    assert!(page.contains("width=\"1600\""));
    assert!(page.contains("height=\"900\""));

    // 6. Replacing flips to a new key and deletes the superseded object.
    let response = app
        .clone()
        .oneshot(multipart_request(
            TIMELINE_ID,
            SESSION_ID,
            &png(1200, 630),
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let replace_hero = body_text(response).await;
    let replacement = media_src(&replace_hero);
    assert_ne!(replacement, key, "a replacement is a new immutable key");
    assert_eq!(attribute(&replace_hero, "width"), "1200");
    assert!(
        !replace_hero.contains("Photo: Demo Author"),
        "the old credit is gone: {replace_hero}"
    );
    assert!(
        s3.head_object()
            .bucket(media_bucket)
            .key(&key)
            .send()
            .await
            .is_err(),
        "the superseded object was deleted"
    );
    assert!(
        s3.head_object()
            .bucket(media_bucket)
            .key(&replacement)
            .send()
            .await
            .is_ok()
    );

    // 7. Removing clears the attribute and the hero falls back to the gradient.
    let response = app
        .clone()
        .oneshot(remove_request(TIMELINE_ID, SESSION_ID)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let gradient_hero = body_text(response).await;
    assert!(
        gradient_hero.starts_with("<header ") && gradient_hero.contains("hero hero--gradient"),
        "{gradient_hero}"
    );
    assert!(!gradient_hero.contains("<img"), "{gradient_hero}");

    let item = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?
        .item()
        .cloned()
        .expect("timeline row exists");
    assert!(
        !item.contains_key("coverImage"),
        "remove clears the coverImage attribute"
    );

    let page = object_text(s3, site_bucket, &format!("timeline/{TIMELINE_ID}.html"))
        .await?
        .expect("timeline page was re-rendered");
    assert!(page.contains("hero hero--gradient"), "{page}");
    assert!(!page.contains("class=\"hero__cover\""), "{page}");

    Ok(())
}

/// The `/media/<key>` src as a bare S3 key.
fn media_src(hero: &str) -> String {
    attribute(hero, "src")
        .strip_prefix("/media/")
        .expect("hero src is a /media/ path")
        .to_owned()
}

async fn stored_cover(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<std::collections::HashMap<String, AttributeValue>, Box<dyn Error + Send + Sync>> {
    let item = ddb
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(TIMELINE_ID)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?
        .item()
        .cloned()
        .expect("timeline row exists");
    match item.get("coverImage") {
        Some(AttributeValue::M(cover)) => Ok(cover.clone()),
        other => panic!("coverImage is a map, got {other:?}"),
    }
}

async fn seed_owner(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let now = Timestamp::now();
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: ACCOUNT_ID.to_owned(),
            display_name: "Cover Owner".to_owned(),
            created_at: now.clone(),
            last_seen_at: now,
            refresh_token_ciphertext: None,
        },
    )
    .await
    .map_err(|error| format!("owner account upsert failed: {error}"))?;
    store::upsert_account(
        ddb,
        table,
        &Account {
            sub: STRANGER_ID.to_owned(),
            display_name: "Cover Stranger".to_owned(),
            created_at: Timestamp::now(),
            last_seen_at: Timestamp::now(),
            refresh_token_ciphertext: None,
        },
    )
    .await
    .map_err(|error| format!("stranger account upsert failed: {error}"))?;

    for (session_id, account_id) in [(SESSION_ID, ACCOUNT_ID), (STRANGER_SESSION_ID, STRANGER_ID)] {
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
    }
    Ok(())
}

async fn seed_timeline(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let now = Timestamp::now();
    let timeline = Timeline {
        id: TIMELINE_ID.to_owned(),
        title: "Cover Integration Timeline".to_owned(),
        description: "A timeline that gains and loses a cover.".to_owned(),
        owner_id: ACCOUNT_ID.to_owned(),
        collection_id: None,
        event_count: 0,
        upvotes: 0,
        downvotes: 0,
        created_at: now.clone(),
        updated_at: now,
        report_count: 0,
        flagged: false,
        published: true,
        cover_image: None,
    };
    ddb.put_item()
        .table_name(table)
        .set_item(Some(timeline.to_item()))
        .send()
        .await?;
    Ok(())
}
