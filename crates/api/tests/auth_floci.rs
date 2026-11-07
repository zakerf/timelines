//! Auth persistence against floci at `AWS_ENDPOINT_URL` (default localhost:4566).
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p api --features floci --test auth_floci -- --ignored
//! ```
//!
//! The test is ignored by default so `cargo test --workspace` stays green
//! without floci running. It exercises the real ACCOUNT upsert, SESSION
//! create/read/delete, the short-lived PKCE handshake row, and KMS round-trip.

#![cfg(feature = "floci")]

use std::error::Error;

use api::auth::cipher::RefreshTokenCipher;
use api::auth::store;
use aws_sdk_kms::config::{Credentials, Region};
use data::model::{Account, AuthState, Session};
use data::{Timestamp, fixture, keys};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn kms_client() -> aws_sdk_kms::Client {
    let region = std::env::var("AWS_DEFAULT_REGION").unwrap_or_else(|_| "eu-central-1".to_owned());
    let config = aws_sdk_kms::Config::builder()
        .behavior_version(aws_sdk_kms::config::BehaviorVersion::latest())
        .region(Region::new(region))
        .credentials_provider(Credentials::new(
            std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "test".to_owned()),
            std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "test".to_owned()),
            None,
            None,
            "floci",
        ))
        .endpoint_url(fixture::endpoint_url())
        .build();
    aws_sdk_kms::Client::from_conf(config)
}

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn upserts_an_account_and_creates_deletes_a_session() -> TestResult {
    let ddb = fixture::client();
    let table = format!(
        "{}-auth-{}",
        fixture::DEFAULT_TABLE_NAME,
        std::process::id()
    );

    fixture::create_table(&ddb, &table).await?;
    let outcome = run(&ddb, &table).await;
    let _ = fixture::delete_table(&ddb, &table).await;
    outcome
}

async fn run(ddb: &aws_sdk_dynamodb::Client, table: &str) -> TestResult {
    // 1. KMS round-trip (floci has a KMS service).
    let kms = kms_client();
    let created = kms
        .create_key()
        .description("timeline auth integration test")
        .send()
        .await?;
    let key_id = created
        .key_metadata()
        .expect("create_key returns metadata")
        .key_id()
        .to_owned();
    let cipher = RefreshTokenCipher::Kms {
        client: kms,
        key_id,
    };

    let encrypted = cipher.encrypt("refresh-token-1").await?;
    assert!(encrypted.starts_with("kms:"));
    assert_eq!(cipher.decrypt(&encrypted).await?, "refresh-token-1");

    // 2. ACCOUNT upsert preserves createdAt on the second login.
    let first_login = Account {
        sub: "user-123".to_owned(),
        display_name: "Ada".to_owned(),
        created_at: timestamp("2026-11-07T12:00:00Z"),
        last_seen_at: timestamp("2026-11-07T12:00:00Z"),
        refresh_token_ciphertext: Some(encrypted.clone()),
    };
    store::upsert_account(ddb, table, &first_login).await?;

    let second_login = Account {
        display_name: "Ada Lovelace".to_owned(),
        created_at: timestamp("2026-11-08T12:00:00Z"),
        last_seen_at: timestamp("2026-11-08T12:00:00Z"),
        ..first_login.clone()
    };
    store::upsert_account(ddb, table, &second_login).await?;

    let stored = store::get_account(ddb, table, "user-123")
        .await?
        .expect("account exists after login");
    assert_eq!(stored.display_name, "Ada Lovelace");
    assert_eq!(
        stored.created_at,
        timestamp("2026-11-07T12:00:00Z"),
        "first login wins createdAt"
    );
    assert_eq!(stored.last_seen_at, timestamp("2026-11-08T12:00:00Z"));
    assert_eq!(
        stored.refresh_token_ciphertext.as_deref(),
        Some(encrypted.as_str())
    );

    // The row uses the PRD key layout.
    let raw = ddb
        .get_item()
        .table_name(table)
        .key(
            "PK",
            aws_sdk_dynamodb::types::AttributeValue::S(keys::account_pk("user-123")),
        )
        .key(
            "SK",
            aws_sdk_dynamodb::types::AttributeValue::S(keys::ACCOUNT_SK.to_owned()),
        )
        .send()
        .await?;
    assert!(raw.item().is_some(), "ACCOUNT#<sub> row landed");

    // 3. SESSION create -> read -> delete.
    let session = Session {
        id: "session-1".to_owned(),
        account_id: "user-123".to_owned(),
        expires_at: 1_800_000_000,
    };
    store::create_session(ddb, table, &session).await?;
    assert_eq!(
        store::get_session(ddb, table, "session-1").await?,
        Some(session)
    );

    store::delete_session(ddb, table, "session-1").await?;
    assert_eq!(store::get_session(ddb, table, "session-1").await?, None);

    // 4. The PKCE handshake row is single-use.
    let handshake = AuthState {
        state: "state-1".to_owned(),
        code_verifier: "verifier-1".to_owned(),
        redirect_uri: "http://localhost:8081/api/auth/callback".to_owned(),
        created_at: timestamp("2026-11-07T12:00:00Z"),
        expires_at: 1_800_000_000,
    };
    store::put_auth_state(ddb, table, &handshake).await?;
    assert_eq!(
        store::take_auth_state(ddb, table, "state-1").await?,
        Some(handshake)
    );
    assert_eq!(store::take_auth_state(ddb, table, "state-1").await?, None);

    Ok(())
}
