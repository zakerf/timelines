//! The API front door: an axum router mounted at `/api/*`.
//!
//! The HTTP API `$default` stage adds no path prefix, so the same router serves
//! both the deployed Lambda and a plain local HTTP process. Auth endpoints live
//! under `/api/auth/*` (see [`auth`]); mutation routes are added by later
//! tickets and take [`AppState`] as their state.

pub mod auth;
pub mod config;
pub mod publish;

use std::sync::Arc;
use std::time::Duration;

use aws_sdk_cloudfront::Client as CloudFrontClient;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_kms::Client as KmsClient;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_ssm::Client as SsmClient;
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tower_http::trace::TraceLayer;

use auth::cipher::RefreshTokenCipher;
use config::{ApiConfig, ConfigError, OidcConfig, RuntimeConfig};

/// Shared request state. Cloneable and cheap; SSM values are cached behind a
/// [`OnceCell`] so startup never depends on floci.
#[derive(Clone)]
pub struct AppState {
    config: Arc<ApiConfig>,
    ssm: Option<SsmClient>,
    ddb: Option<DynamoClient>,
    kms: Option<KmsClient>,
    s3: Option<S3Client>,
    cloudfront: Option<CloudFrontClient>,
    http: reqwest::Client,
    runtime: Arc<OnceCell<RuntimeConfig>>,
    oidc: Arc<OnceCell<OidcConfig>>,
}

impl AppState {
    pub fn new(config: ApiConfig, ssm: Option<SsmClient>) -> Self {
        Self {
            config: Arc::new(config),
            ssm,
            ddb: None,
            kms: None,
            s3: None,
            cloudfront: None,
            http: http_client(),
            runtime: Arc::new(OnceCell::new()),
            oidc: Arc::new(OnceCell::new()),
        }
    }

    /// Full local/deployed state with the DynamoDB and KMS clients attached.
    pub fn with_aws(
        config: ApiConfig,
        ssm: Option<SsmClient>,
        ddb: Option<DynamoClient>,
        kms: Option<KmsClient>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            ssm,
            ddb,
            kms,
            s3: None,
            cloudfront: None,
            http: http_client(),
            runtime: Arc::new(OnceCell::new()),
            oidc: Arc::new(OnceCell::new()),
        }
    }

    /// Attach the renderer's S3 writer and the CloudFront invalidator. The
    /// write path needs both; tests that only read/write DynamoDB can omit them.
    pub fn with_storage(
        mut self,
        s3: Option<S3Client>,
        cloudfront: Option<CloudFrontClient>,
    ) -> Self {
        self.s3 = s3;
        self.cloudfront = cloudfront;
        self
    }

    /// State for plain local HTTP (and tests) with no AWS client attached.
    pub fn local(config: ApiConfig) -> Self {
        Self::new(config, None)
    }

    /// Local state with the OIDC seam already resolved, for route tests.
    pub fn local_with_runtime(config: ApiConfig, runtime: RuntimeConfig) -> Self {
        let state = Self::local(config);
        let _ = state.oidc.set(runtime.oidc.clone());
        let _ = state.runtime.set(runtime);
        state
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn dynamodb_opt(&self) -> Option<&DynamoClient> {
        self.ddb.as_ref()
    }

    pub fn dynamodb(&self) -> Result<&DynamoClient, ConfigError> {
        self.ddb.as_ref().ok_or(ConfigError::MissingDynamoClient)
    }

    pub fn s3_opt(&self) -> Option<&S3Client> {
        self.s3.as_ref()
    }

    pub fn s3(&self) -> Result<&S3Client, ConfigError> {
        self.s3.as_ref().ok_or(ConfigError::MissingS3Client)
    }

    pub fn cloudfront_opt(&self) -> Option<&CloudFrontClient> {
        self.cloudfront.as_ref()
    }

    pub fn kms_opt(&self) -> Option<&KmsClient> {
        self.kms.as_ref()
    }

    /// The refresh-token cipher: KMS when a key is configured, local fallback
    /// otherwise.
    pub fn cipher(&self) -> RefreshTokenCipher {
        match (self.kms.as_ref(), self.config.kms_key_id.as_ref()) {
            (Some(client), Some(key_id)) => RefreshTokenCipher::Kms {
                client: client.clone(),
                key_id: key_id.clone(),
            },
            _ => RefreshTokenCipher::Local,
        }
    }

    /// Resolve SSM-backed configuration exactly once.
    pub async fn runtime_config(&self) -> Result<&RuntimeConfig, ConfigError> {
        self.runtime
            .get_or_try_init(|| async {
                let ssm = self.ssm.as_ref().ok_or(ConfigError::MissingSsmClient)?;
                config::load_runtime_config(&self.config, ssm).await
            })
            .await
    }

    /// Resolve just the OIDC seam; auth works even if the CDN parameter is not
    /// present yet.
    pub async fn oidc_config(&self) -> Result<&OidcConfig, ConfigError> {
        if let Some(runtime) = self.runtime.get() {
            return Ok(&runtime.oidc);
        }
        self.oidc
            .get_or_try_init(|| async {
                if let Some(runtime) = self.runtime.get() {
                    return Ok(runtime.oidc.clone());
                }
                let ssm = self.ssm.as_ref().ok_or(ConfigError::MissingSsmClient)?;
                config::load_oidc_config(&self.config, ssm).await
            })
            .await
    }

    /// Attempt the init-time SSM read. Callers decide whether failure is fatal;
    /// local HTTP treats it as a warning so health stays up without floci.
    pub async fn warm(&self) -> Result<&RuntimeConfig, ConfigError> {
        self.runtime_config().await
    }
}

/// An OIDC HTTP client with a bounded timeout, so a slow provider cannot hold
/// a request (including logout's best-effort revocation) open forever.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the OIDC HTTP client configuration is valid")
}

/// Build the application router. Every route lives under `/api/*`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .nest("/api", api_routes())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .nest("/auth", auth::routes())
        .merge(publish::routes())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// Liveness only: no table access, no SSM call, no floci requirement.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
