//! Plain-HTTP entry point for local development.
//!
//! The deployed artifact is the same router packaged as a Lambda `bootstrap`;
//! locally `cargo run -p api` serves it over HTTP so `/api/health` can be
//! curled without an emulated Lambda runtime.

use std::error::Error;

use api::config::ApiConfig;
use api::{AppState, app};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let config = ApiConfig::from_env()?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let ssm = aws_sdk_ssm::Client::new(&sdk_config);
    let state = AppState::new(config, Some(ssm));

    match state.warm().await {
        Ok(runtime) => tracing::info!(
            distribution_id = %runtime.distribution_id,
            oidc_client_id = %runtime.oidc.client_id,
            "resolved SSM configuration"
        ),
        Err(error) => tracing::warn!(
            %error,
            "SSM configuration unavailable; starting anyway so /api/health stays up"
        ),
    }

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_owned());
    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "api listening (plain HTTP)");

    axum::serve(listener, app(state)).await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
