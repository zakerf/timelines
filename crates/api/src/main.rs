//! Plain-HTTP entry point for local development.
//!
//! The deployed artifact is the same router packaged as a Lambda `bootstrap`;
//! locally `cargo run -p api` serves it over HTTP so `/api/health` can be
//! curled without an emulated Lambda runtime.
//!
//! Keycloak owns localhost:8080, so the API defaults to 8081. `PUBLIC_BASE_URL`
//! must match the browser origin: the OIDC redirect URI is derived from it.

use std::error::Error;

use api::config::ApiConfig;
use api::{AppState, app};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8081";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let config = ApiConfig::from_env()?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let ssm = aws_sdk_ssm::Client::new(&sdk_config);
    let ddb = aws_sdk_dynamodb::Client::new(&sdk_config);
    let kms = aws_sdk_kms::Client::new(&sdk_config);
    let s3 = aws_sdk_s3::Client::new(&sdk_config);
    let cloudfront = aws_sdk_cloudfront::Client::new(&sdk_config);
    let state = AppState::with_aws(config, Some(ssm), Some(ddb), Some(kms))
        .with_storage(Some(s3), Some(cloudfront));

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

    tracing::info!(
        public_base_url = %state.config().public_base_url,
        kms = if state.config().kms_key_id.is_some() {
            "configured"
        } else {
            "local fallback"
        },
        "auth seam configured"
    );

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
