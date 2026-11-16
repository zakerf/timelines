//! GSI2 membership and the adjacency subtree walk against floci.
//!
//! Run it with:
//!
//! ```sh
//! docker compose up -d floci
//! cargo test -p data --features fixture --test browse_floci -- --ignored
//! ```
//!
//! Ignored by default so `cargo test --workspace` stays green without floci.

#![cfg(feature = "fixture")]

use std::error::Error;

use aws_sdk_dynamodb::Client;
use data::browse::{collection_subtree, list_contents};
use data::item::DynamoItem;
use data::model::{Collection, Timeline};
use data::{Timestamp, fixture};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).expect("test timestamp is valid")
}

fn collection(id: &str, title: &str, parent: Option<&str>) -> Collection {
    Collection {
        id: id.to_owned(),
        title: title.to_owned(),
        description: format!("{title} description"),
        parent_collection_id: parent.map(str::to_owned),
        created_at: timestamp("2026-01-01T00:00:00Z"),
        created_by: "owner-1".to_owned(),
    }
}

fn timeline(id: &str, title: &str, collection_id: Option<&str>) -> Timeline {
    Timeline {
        id: id.to_owned(),
        title: title.to_owned(),
        description: format!("{title} deck"),
        owner_id: "owner-1".to_owned(),
        collection_id: collection_id.map(str::to_owned),
        event_count: 1,
        upvotes: 0,
        downvotes: 0,
        created_at: timestamp("2026-01-01T00:00:00Z"),
        updated_at: timestamp("2026-01-02T00:00:00Z"),
        report_count: 0,
        flagged: false,
        published: true,
        cover_image: None,
    }
}

/// Root A -> child B -> grandchild C, with one Timeline at each level and one
/// uncollected Timeline at ROOT.
async fn seed(client: &Client, table: &str) -> TestResult {
    let rows: Vec<std::collections::HashMap<String, aws_sdk_dynamodb::types::AttributeValue>> = vec![
        collection("a", "Collection A", None).to_item(),
        collection("b", "Collection B", Some("a")).to_item(),
        collection("c", "Collection C", Some("b")).to_item(),
        timeline("t-a", "Timeline in A", Some("a")).to_item(),
        timeline("t-b", "Timeline in B", Some("b")).to_item(),
        timeline("t-c", "Timeline in C", Some("c")).to_item(),
        timeline("t-root", "Uncollected Timeline", None).to_item(),
    ];
    for item in rows {
        client
            .put_item()
            .table_name(table)
            .set_item(Some(item))
            .send()
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn gsi2_lists_a_collections_children_and_timelines_separately() -> TestResult {
    let client = fixture::client();
    let table = format!(
        "{}-browse-{}-{}",
        fixture::DEFAULT_TABLE_NAME,
        std::process::id(),
        unix_nanos()
    );
    fixture::create_table(&client, &table).await?;

    let outcome = async {
        seed(&client, &table).await?;

        let contents = list_contents(&client, &table, Some("a")).await?;
        assert_eq!(
            contents
                .collections
                .iter()
                .map(|collection| collection.id.as_str())
                .collect::<Vec<_>>(),
            ["b"],
            "only direct child collections are in A's partition"
        );
        assert_eq!(
            contents
                .timelines
                .iter()
                .map(|timeline| timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["t-a"],
            "only Timelines assigned to A are in A's partition"
        );

        let root = list_contents(&client, &table, None).await?;
        assert_eq!(
            root.collections
                .iter()
                .map(|collection| collection.id.as_str())
                .collect::<Vec<_>>(),
            ["a"],
            "ROOT holds top-level collections"
        );
        assert_eq!(
            root.timelines
                .iter()
                .map(|timeline| timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["t-root"],
            "ROOT holds uncollected Timelines"
        );

        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;

    let _ = fixture::delete_table(&client, &table).await;
    outcome
}

#[tokio::test]
#[ignore = "requires floci: docker compose up -d floci"]
async fn subtree_walks_adjacency_through_every_nested_collection() -> TestResult {
    let client = fixture::client();
    let table = format!(
        "{}-subtree-{}-{}",
        fixture::DEFAULT_TABLE_NAME,
        std::process::id(),
        unix_nanos()
    );
    fixture::create_table(&client, &table).await?;

    let outcome = async {
        seed(&client, &table).await?;

        let subtree = collection_subtree(&client, &table, "a").await?;
        assert_eq!(subtree.collection.id, "a");
        assert_eq!(
            subtree
                .timelines
                .iter()
                .map(|timeline| timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["t-a"]
        );
        assert_eq!(subtree.children.len(), 1);
        let b = &subtree.children[0];
        assert_eq!(b.collection.id, "b");
        assert_eq!(
            b.timelines
                .iter()
                .map(|timeline| timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["t-b"]
        );
        assert_eq!(b.children.len(), 1);
        let c = &b.children[0];
        assert_eq!(c.collection.id, "c");
        assert_eq!(
            c.timelines
                .iter()
                .map(|timeline| timeline.id.as_str())
                .collect::<Vec<_>>(),
            ["t-c"]
        );
        assert!(c.children.is_empty(), "the walk stops at a leaf");

        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;

    let _ = fixture::delete_table(&client, &table).await;
    outcome
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos()
}
