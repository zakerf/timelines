//! Seed a demo Timeline into floci (or any table) for a local end-to-end render.
//!
//! ```sh
//! docker compose up -d floci
//! $env:AWS_ENDPOINT_URL = "http://localhost:4566"
//! cargo run -p renderer --bin seed -- --table timeline-data
//! cargo run -p renderer -- --table timeline-data --out target/site
//! ```
//!
//! The table is created with the same shape as the `data` OpenTofu module when
//! it does not exist. Seeding is idempotent: rows are overwritten, not appended.

use std::env;
use std::error::Error;
use std::io;

use aws_sdk_dynamodb::Client;
use data::item::DynamoItem;
use renderer::dynamo;

const USAGE: &str = "\
usage: seed [--table <name>]

  --table  DynamoDB table name (default: $TABLE_NAME or timeline-data)

Environment: AWS_ENDPOINT_URL (floci), AWS_REGION / AWS_DEFAULT_REGION,
AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY.";

struct Args {
    table: String,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut args = Self {
            table: env::var("TABLE_NAME").unwrap_or_else(|_| dynamo::DEFAULT_TABLE_NAME.to_owned()),
        };

        let mut values = env::args().skip(1);
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--table" => {
                    args.table = values.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("--table needs a value\n\n{USAGE}"),
                        )
                    })?;
                }
                "--help" | "-h" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument {other:?}\n\n{USAGE}"),
                    )
                    .into());
                }
            }
        }

        Ok(args)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args = Args::parse()?;
    let client = dynamo::client().await;

    let created = dynamo::ensure_table(&client, &args.table).await?;
    println!(
        "{} table {:?}",
        if created { "created" } else { "using existing" },
        args.table
    );

    let rows = dynamo::demo_rows()?;
    put(&client, &args.table, rows.account.to_item()).await?;
    put(&client, &args.table, rows.collection.to_item()).await?;
    put(&client, &args.table, rows.timeline.to_item()).await?;
    for event in rows.events {
        put(&client, &args.table, event.to_item()).await?;
    }

    println!(
        "seeded timeline {:?} ({} events) into {:?}",
        rows.timeline.id, rows.timeline.event_count, args.table
    );
    println!(
        "render it with: cargo run -p renderer -- --table {} --out target/site",
        args.table
    );

    Ok(())
}

async fn put(
    client: &Client,
    table: &str,
    item: std::collections::HashMap<String, aws_sdk_dynamodb::types::AttributeValue>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;
    Ok(())
}
