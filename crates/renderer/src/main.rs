//! One-shot SSG binary: read DynamoDB, render the whole site, write `.html`.
//!
//! ```sh
//! cargo run -p renderer -- --out target/site
//! cargo run -p renderer -- --timeline ohio-senate-2026 --out target/site
//! ```
//!
//! `AWS_ENDPOINT_URL` selects floci locally; leave it unset on AWS.

use std::env;
use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use leptos::config::LeptosOptions;
use renderer::{dynamo, generate_site, init_executor};

const USAGE: &str = "\
usage: renderer [--out <dir>] [--table <name>] [--timeline <id>]

  --out       output directory for rendered pages (default: target/site)
  --table     DynamoDB table name (default: $TABLE_NAME or timeline-data)
  --timeline  render only this Timeline id (default: every Timeline)

Environment: AWS_ENDPOINT_URL (floci), AWS_REGION / AWS_DEFAULT_REGION,
AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY.";

struct Args {
    out: PathBuf,
    table: String,
    timeline: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut args = Self {
            out: PathBuf::from("target/site"),
            table: env::var("TABLE_NAME").unwrap_or_else(|_| dynamo::DEFAULT_TABLE_NAME.to_owned()),
            timeline: None,
        };

        let mut values = env::args().skip(1);
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--out" => args.out = PathBuf::from(next_value(&mut values, "--out")?),
                "--table" => args.table = next_value(&mut values, "--table")?,
                "--timeline" => {
                    args.timeline = Some(next_value(&mut values, "--timeline")?);
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

fn next_value(
    values: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    values.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} needs a value\n\n{USAGE}"),
        )
        .into()
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args = Args::parse()?;

    // Install the custom SSG executor before any Leptos reactive API runs.
    init_executor();

    let client = dynamo::client().await;
    let site = dynamo::load_site(&client, &args.table, args.timeline.as_deref()).await?;
    if site.pages.is_empty() {
        // A brand-new environment has no Timelines. Home still renders (with its
        // empty state and publish call to action), so the first deploy works
        // before anyone has published anything.
        eprintln!(
            "table {:?} has no timelines; rendering Home with the empty state \
             (seed floci with `cargo run -p renderer --bin seed -- --table {}`)",
            args.table, args.table
        );
    }

    let options = LeptosOptions::builder()
        .output_name("timelines")
        .site_root(args.out.to_string_lossy().to_string())
        .site_addr(SocketAddr::from(([0, 0, 0, 0], 3000)))
        .build();

    let generated = generate_site(&site, &options).await?;
    println!(
        "rendered {} page(s) from table {:?} into {}",
        generated.len(),
        args.table,
        args.out.display()
    );
    for page in &generated {
        println!(
            "  {} -> {} ({} bytes)",
            page.route_path,
            page.file_path.display(),
            page.bytes
        );
    }

    Ok(())
}
