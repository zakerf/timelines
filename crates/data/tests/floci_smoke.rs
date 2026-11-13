//! Smoke test against floci at `AWS_ENDPOINT_URL` (default localhost:4566).
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p data --features fixture --test floci_smoke -- --ignored
//! ```
//!
//! The test is ignored by default so `cargo test --workspace` stays green
//! without floci running.

#![cfg(feature = "fixture")]

use std::error::Error;

use aws_sdk_dynamodb::types::AttributeValue;
use data::item::DynamoItem;
use data::model::{
    EditPayload, Emphasis, Event, Suggestion, SuggestionKind, SuggestionPayload, SuggestionState,
    Timeline, Vote, VoteDirection,
};
use data::{Timestamp, fixture, keys};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

fn sample_timeline() -> Timeline {
    Timeline {
        id: "t1".to_owned(),
        title: "Smoke timeline".to_owned(),
        description: "Written by the floci smoke test".to_owned(),
        owner_id: "owner-1".to_owned(),
        collection_id: None,
        event_count: 1,
        upvotes: 0,
        downvotes: 0,
        created_at: timestamp("2026-11-07T12:00:00Z"),
        updated_at: timestamp("2026-11-07T12:00:00Z"),
        report_count: 0,
        flagged: false,
        cover_image: None,
    }
}

fn sample_event() -> Event {
    Event {
        timeline_id: "t1".to_owned(),
        event_id: "e1".to_owned(),
        timestamp: timestamp("2026-11-07T12:30:00Z"),
        date: "2026-11-07".to_owned(),
        title: "Smoke event".to_owned(),
        description: "An event written to the real table".to_owned(),
        sources: vec!["https://example.com/source".to_owned()],
        emphasis: Emphasis::Featured,
        upvotes: 0,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        created_at: timestamp("2026-11-07T12:31:00Z"),
        created_by: "owner-1".to_owned(),
        last_edited_by: None,
        last_edit_suggestion_id: None,
    }
}

fn sample_suggestion() -> Suggestion {
    Suggestion {
        suggestion_id: "s1".to_owned(),
        timeline_id: "t1".to_owned(),
        owner_id: Some("owner-1".to_owned()),
        state: SuggestionState::Pending,
        kind: SuggestionKind::Edit,
        target_event_id: Some("e1".to_owned()),
        payload: SuggestionPayload::Edit(EditPayload {
            title: Some("Smoke event, corrected".to_owned()),
            description: None,
            date: None,
            sources: None,
            emphasis: None,
        }),
        submitted_by: "account-9".to_owned(),
        submitted_at: timestamp("2026-11-07T13:00:00Z"),
        reviewed_by: None,
        reviewed_at: None,
        review_note: None,
        report_count: 0,
        flagged: false,
    }
}

fn sample_vote() -> Vote {
    Vote {
        account_id: "account-9".to_owned(),
        timeline_id: "t1".to_owned(),
        event_id: None,
        direction: VoteDirection::Up,
        created_at: timestamp("2026-11-07T13:01:00Z"),
    }
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn writes_and_queries_a_timeline_event_suggestion_and_vote() -> TestResult {
    let client = fixture::client();
    let table = format!("{}-{}", fixture::DEFAULT_TABLE_NAME, std::process::id());

    fixture::create_table(&client, &table).await?;
    let outcome = run_smoke(&client, &table).await;
    // Best-effort cleanup; a failed assertion must not hide the original error.
    let _ = fixture::delete_table(&client, &table).await;
    outcome
}

async fn run_smoke(client: &aws_sdk_dynamodb::Client, table: &str) -> TestResult {
    let timeline = sample_timeline();
    let event = sample_event();
    let suggestion = sample_suggestion();
    let vote = sample_vote();

    client
        .put_item()
        .table_name(table)
        .set_item(Some(timeline.to_item()))
        .send()
        .await?;
    client
        .put_item()
        .table_name(table)
        .set_item(Some(event.to_item()))
        .send()
        .await?;

    // Access pattern 1: events chronological within the timeline partition.
    let events = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(keys::event_pk("t1")))
        .expression_attribute_values(
            ":prefix",
            AttributeValue::S(keys::EVENT_SK_PREFIX.to_owned()),
        )
        .send()
        .await?;
    assert_eq!(events.items().len(), 1, "expected exactly one event");
    assert_eq!(Event::from_item(events.items()[0].clone())?, event);

    // Access pattern 9/5: a pending suggestion is immediately in the owner queue.
    client
        .put_item()
        .table_name(table)
        .set_item(Some(suggestion.to_item()))
        .send()
        .await?;
    let queue = client
        .query()
        .table_name(table)
        .index_name("GSI1")
        .key_condition_expression("GSI1PK = :pk")
        .expression_attribute_values(
            ":pk",
            AttributeValue::S(keys::suggestion_gsi1_pk("owner-1")),
        )
        .send()
        .await?;
    assert_eq!(queue.items().len(), 1, "expected one pending suggestion");
    let queued = Suggestion::from_item(queue.items()[0].clone())?;
    assert_eq!(queued.suggestion_id, "s1");
    assert_eq!(queued.state, SuggestionState::Pending);

    // Access pattern 7 + 3: one vote item; own-vote state in one query.
    client
        .put_item()
        .table_name(table)
        .set_item(Some(vote.to_item()))
        .send()
        .await?;
    let own_votes = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(keys::vote_pk("account-9")))
        .expression_attribute_values(
            ":prefix",
            AttributeValue::S(keys::vote_timeline_prefix("t1")),
        )
        .send()
        .await?;
    assert_eq!(own_votes.items().len(), 1, "expected one own vote");
    let read_vote = Vote::from_item(own_votes.items()[0].clone())?;
    assert_eq!(read_vote, vote);
    assert_eq!(read_vote.direction, VoteDirection::Up);

    // The timeline itself is readable at its own key.
    let stored = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(keys::timeline_pk("t1")))
        .key("SK", AttributeValue::S(keys::TIMELINE_SK.to_owned()))
        .send()
        .await?;
    let stored_timeline =
        Timeline::from_item(stored.item().expect("timeline item exists").clone())?;
    assert_eq!(stored_timeline, timeline);

    Ok(())
}
