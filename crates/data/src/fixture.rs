//! Table fixture for integration tests against floci.
//!
//! It builds the same table shape the `modules/data` OpenTofu module creates:
//! `PAY_PER_REQUEST`, composite `PK`/`SK`, and GSI1/GSI2 with projection ALL.
//! TTL, PITR, deletion protection, and the alarm are infrastructure concerns
//! verified by `tofu validate`/`tofu plan`, not by this fixture.
//!
//! Enable the `fixture` feature to compile this module.

use std::env;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType, Projection,
    ProjectionType, ScalarAttributeType,
};

/// Default table name; tests append a unique suffix.
pub const DEFAULT_TABLE_NAME: &str = "timeline-data";

/// Endpoint convention shared by local dev and CI: `AWS_ENDPOINT_URL`.
pub fn endpoint_url() -> String {
    env::var("AWS_ENDPOINT_URL").unwrap_or_else(|_| "http://localhost:4566".to_owned())
}

fn credentials() -> Credentials {
    Credentials::new(
        env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "test".to_owned()),
        env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "test".to_owned()),
        None,
        None,
        "floci",
    )
}

/// A DynamoDB client pointed at floci with dummy credentials.
pub fn client() -> Client {
    let region = env::var("AWS_DEFAULT_REGION").unwrap_or_else(|_| "eu-central-1".to_owned());
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region))
        .credentials_provider(credentials())
        .endpoint_url(endpoint_url())
        .build();
    Client::from_conf(config)
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

/// Create the single table. The caller chooses a unique name so repeated local
/// runs never collide.
pub async fn create_table(
    client: &Client,
    table_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    client
        .create_table()
        .table_name(table_name)
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

/// Best-effort teardown for tests.
pub async fn delete_table(
    client: &Client,
    table_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    client.delete_table().table_name(table_name).send().await?;
    Ok(())
}
