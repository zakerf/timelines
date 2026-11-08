//! DynamoDB read path for the renderer, plus the local seed helpers.
//!
//! Uses the data crate's item types and key builders, so the renderer reads
//! exactly the single-table layout in PRD §6. `AWS_ENDPOINT_URL` is the only
//! switch between floci and real AWS.

use std::env;
use std::error::Error;
use std::io;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, Projection, ProjectionType, ScalarAttributeType,
};
use data::Timestamp;
use data::item::{DynamoItem, ItemType};
use data::keys;
use data::model::{Account, Collection, Event, Timeline};

use crate::page::{Site, TimelinePage};

/// Result type shared by the read and seed paths.
pub type StoreResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Table name used by the local fixture and the seed binary.
pub const DEFAULT_TABLE_NAME: &str = "timeline-data";

/// Fallback region when neither AWS_REGION nor AWS_DEFAULT_REGION is set.
const DEFAULT_REGION: &str = "eu-central-1";

/// A DynamoDB client that honors `AWS_ENDPOINT_URL` (floci) or the normal AWS
/// credential chain when it is unset.
pub async fn client() -> Client {
    let region = env::var("AWS_REGION")
        .or_else(|_| env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| DEFAULT_REGION.to_owned());
    let endpoint = env::var("AWS_ENDPOINT_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());

    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(Region::new(region));
    // Local floci accepts any credentials; supply the documented test pair when
    // the caller did not export one, so a local render never stalls on IMDS.
    if endpoint.is_some() && env::var("AWS_ACCESS_KEY_ID").is_err() {
        loader = loader.credentials_provider(Credentials::new("test", "test", None, None, "floci"));
    }
    let sdk_config = loader.load().await;

    let mut builder = aws_sdk_dynamodb::config::Builder::from(&sdk_config);
    if let Some(endpoint) = endpoint {
        builder = builder.endpoint_url(endpoint);
    }
    Client::from_conf(builder.build())
}

/// Load every Timeline (or just `only`) with its ordered Events, owner byline,
/// and collection eyebrow.
pub async fn load_site(client: &Client, table: &str, only: Option<&str>) -> StoreResult<Site> {
    let timelines = match only {
        Some(id) => vec![load_timeline(client, table, id).await?],
        None => scan_timelines(client, table).await?,
    };
    let mut collections = scan_collections(client, table).await?;
    collections.sort_by(|left, right| left.id.cmp(&right.id));

    let mut pages = Vec::with_capacity(timelines.len());
    for timeline in timelines {
        let events = load_events(client, table, &timeline.id).await?;
        let owner_name = load_owner_name(client, table, &timeline.owner_id).await?;
        let collection_title = match &timeline.collection_id {
            Some(collection_id) => load_collection_title(client, table, collection_id).await?,
            None => None,
        };
        pages.push(TimelinePage {
            timeline,
            events,
            owner_name,
            collection_title,
        });
    }
    pages.sort_by(|left, right| left.timeline.id.cmp(&right.timeline.id));

    Ok(Site { pages, collections })
}

/// Get one Timeline metadata row.
pub async fn load_timeline(client: &Client, table: &str, id: &str) -> StoreResult<Timeline> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk(id)))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?;

    let item = output
        .item()
        .ok_or_else(|| not_found(format!("timeline {id:?} is not published")))?;
    Ok(Timeline::from_item(item.clone())?)
}

/// Scan the table for TIMELINE rows. MVP has no all-timelines index; a filtered
/// scan is the read path for a full-site render.
pub async fn scan_timelines(client: &Client, table: &str) -> StoreResult<Vec<Timeline>> {
    let mut timelines = Vec::new();
    let mut start_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .scan()
            .table_name(table)
            .filter_expression("#item_type = :timeline")
            .expression_attribute_names("#item_type", "itemType")
            .expression_attribute_values(
                ":timeline",
                AttributeValue::S(ItemType::Timeline.as_str().to_owned()),
            );
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request.send().await?;

        for item in output.items() {
            timelines.push(Timeline::from_item(item.clone())?);
        }
        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    Ok(timelines)
}

/// Scan the table for COLLECTION rows. Collections are small and shallow, so a
/// full scan is cheaper than a walk from ROOT for a full-site render.
pub async fn scan_collections(client: &Client, table: &str) -> StoreResult<Vec<Collection>> {
    let mut collections = Vec::new();
    let mut start_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .scan()
            .table_name(table)
            .filter_expression("#item_type = :collection")
            .expression_attribute_names("#item_type", "itemType")
            .expression_attribute_values(
                ":collection",
                AttributeValue::S(ItemType::Collection.as_str().to_owned()),
            );
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request.send().await?;

        for item in output.items() {
            collections.push(Collection::from_item(item.clone())?);
        }
        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    Ok(collections)
}

/// Query the timeline partition's EVENT rows. Sort keys are
/// `EVENT#<timestamp>#<id>`, so ascending query order is chronological.
pub async fn load_events(
    client: &Client,
    table: &str,
    timeline_id: &str,
) -> StoreResult<Vec<Event>> {
    let mut events = Vec::new();
    let mut start_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(keys::event_pk(timeline_id)))
            .expression_attribute_values(
                ":prefix",
                AttributeValue::S(keys::EVENT_SK_PREFIX.to_owned()),
            );
        if let Some(key) = start_key {
            request = request.set_exclusive_start_key(Some(key));
        }
        let output = request.send().await?;

        for item in output.items() {
            events.push(Event::from_item(item.clone())?);
        }
        start_key = output.last_evaluated_key().cloned();
        if start_key.is_none() {
            break;
        }
    }

    Ok(events)
}

async fn load_owner_name(
    client: &Client,
    table: &str,
    owner_id: &str,
) -> StoreResult<Option<String>> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::account_pk(owner_id)))
        .key("SK", AttributeValue::S(keys::ACCOUNT_SK.to_owned()))
        .send()
        .await?;

    match output.item() {
        Some(item) => Ok(Some(Account::from_item(item.clone())?.display_name)),
        None => Ok(None),
    }
}

async fn load_collection_title(
    client: &Client,
    table: &str,
    collection_id: &str,
) -> StoreResult<Option<String>> {
    let output = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::collection_pk(collection_id)))
        .key("SK", AttributeValue::S(keys::COLLECTION_SK.to_owned()))
        .send()
        .await?;

    match output.item() {
        Some(item) => Ok(Some(Collection::from_item(item.clone())?.title)),
        None => Ok(None),
    }
}

/// Create the single table if it does not exist yet. Returns `true` when it
/// created the table.
pub async fn ensure_table(client: &Client, table: &str) -> StoreResult<bool> {
    if client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .is_ok()
    {
        return Ok(false);
    }

    create_table(client, table).await?;
    Ok(true)
}

async fn create_table(client: &Client, table: &str) -> StoreResult<()> {
    client
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(attribute("PK", ScalarAttributeType::S))
        .attribute_definitions(attribute("SK", ScalarAttributeType::S))
        .attribute_definitions(attribute("GSI1PK", ScalarAttributeType::S))
        .attribute_definitions(attribute("GSI1SK", ScalarAttributeType::S))
        .attribute_definitions(attribute("GSI2PK", ScalarAttributeType::S))
        .attribute_definitions(attribute("GSI2SK", ScalarAttributeType::S))
        .key_schema(key("PK", KeyType::Hash))
        .key_schema(key("SK", KeyType::Range))
        .global_secondary_indexes(gsi("GSI1", "GSI1PK", "GSI1SK"))
        .global_secondary_indexes(gsi("GSI2", "GSI2PK", "GSI2SK"))
        .send()
        .await?;
    Ok(())
}

fn attribute(name: &str, kind: ScalarAttributeType) -> AttributeDefinition {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(kind)
        .build()
        .expect("static attribute definition is valid")
}

fn key(name: &str, kind: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(kind)
        .build()
        .expect("static key schema is valid")
}

fn gsi(name: &str, hash: &str, range: &str) -> GlobalSecondaryIndex {
    GlobalSecondaryIndex::builder()
        .index_name(name)
        .key_schema(key(hash, KeyType::Hash))
        .key_schema(key(range, KeyType::Range))
        .projection(
            Projection::builder()
                .projection_type(ProjectionType::All)
                .build(),
        )
        .build()
        .expect("static secondary index is valid")
}

/// Demo rows written by the `seed` binary. Fixed ids and dates so a local
/// render is byte-comparable run to run.
pub fn demo_rows() -> StoreResult<DemoRows> {
    let created = Timestamp::parse("2026-01-01T00:00:00Z")?;
    let updated = Timestamp::parse("2026-01-02T00:00:00Z")?;

    let account = Account {
        sub: "demo-author".to_owned(),
        display_name: "Demo Author".to_owned(),
        created_at: created.clone(),
        last_seen_at: updated.clone(),
        refresh_token_ciphertext: None,
    };
    let collection = Collection {
        id: "elections-2026".to_owned(),
        title: "Elections 2026".to_owned(),
        description: "Timelines around the 2026 election cycle.".to_owned(),
        parent_collection_id: None,
        created_at: created.clone(),
        created_by: "demo-author".to_owned(),
    };
    let timeline = Timeline {
        id: "ohio-senate-2026".to_owned(),
        title: "Ohio Senate 2026".to_owned(),
        description: "Key dates in the 2026 Ohio Senate election cycle.".to_owned(),
        owner_id: "demo-author".to_owned(),
        collection_id: Some("elections-2026".to_owned()),
        event_count: 3,
        upvotes: 58,
        downvotes: 10,
        created_at: created.clone(),
        updated_at: updated.clone(),
        report_count: 0,
        flagged: false,
        // No cover: this exercises the deterministic gradient hero locally.
        cover_image: None,
    };

    let event = |event_id: &str,
                 timestamp: &str,
                 date: &str,
                 title: &str,
                 description: &str,
                 sources: &[&str],
                 emphasis: data::model::Emphasis,
                 upvotes: i64,
                 downvotes: i64|
     -> StoreResult<Event> {
        Ok(Event {
            timeline_id: timeline.id.clone(),
            event_id: event_id.to_owned(),
            timestamp: Timestamp::parse(timestamp)?,
            date: date.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            sources: sources.iter().map(|source| (*source).to_owned()).collect(),
            emphasis,
            upvotes,
            downvotes,
            report_count: 0,
            flagged: false,
            created_at: created.clone(),
            created_by: "demo-author".to_owned(),
            last_edited_by: None,
            last_edit_suggestion_id: None,
        })
    };

    let events = vec![
        event(
            "filing-deadline",
            "2026-02-04T00:00:00Z",
            "2026-02-04",
            "Candidate filing deadline",
            "Petitions for the 2026 primary had to be filed with the Ohio Secretary of State by 4 p.m. on February 4th.",
            &["https://www.ohiosos.gov/elections/candidates/"],
            data::model::Emphasis::Standard,
            12,
            1,
        )?,
        event(
            "primary-day",
            "2026-05-05T00:00:00Z",
            "2026-05-05",
            "Primary election day",
            "Party nominees for Ohio's 33 State Senate districts were decided at the May 5th primary.",
            &[
                "https://www.ohiosos.gov/elections/",
                "https://www.example.org/primary-results",
            ],
            data::model::Emphasis::Featured,
            34,
            6,
        )?,
        event(
            "general-day",
            "2026-11-03T00:00:00Z",
            "2026-11-03",
            "General election day",
            "Ohio's 2026 general election; all 33 State Senate seats were on the ballot.",
            &["https://www.ohiosos.gov/elections/results/"],
            data::model::Emphasis::Standard,
            41,
            3,
        )?,
    ];

    Ok(DemoRows {
        account,
        collection,
        timeline,
        events,
    })
}

/// The demo Timeline's rows, ready to `PutItem`.
#[derive(Clone, Debug)]
pub struct DemoRows {
    pub account: Account,
    pub collection: Collection,
    pub timeline: Timeline,
    pub events: Vec<Event>,
}

fn not_found(message: String) -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(io::ErrorKind::NotFound, message))
}
