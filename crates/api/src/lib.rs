//! The API front door: an axum router mounted at `/api/*`.
//!
//! The HTTP API `$default` stage adds no path prefix, so the same router serves
//! both the deployed Lambda and a plain local HTTP process. Auth and mutation
//! routes are added to [`api_routes`] by later tickets; they take [`AppState`]
//! as their state.

pub mod config;

use std::sync::Arc;

use aws_sdk_ssm::Client as SsmClient;
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tower_http::trace::TraceLayer;

use config::{ApiConfig, ConfigError, RuntimeConfig};

/// Shared request state. Cloneable and cheap; the resolved SSM values are
/// cached behind a [`OnceCell`] so startup never depends on floci.
#[derive(Clone)]
pub struct AppState {
    config: Arc<ApiConfig>,
    ssm: Option<SsmClient>,
    runtime: Arc<OnceCell<RuntimeConfig>>,
}

impl AppState {
    pub fn new(config: ApiConfig, ssm: Option<SsmClient>) -> Self {
        Self {
            config: Arc::new(config),
            ssm,
            runtime: Arc::new(OnceCell::new()),
        }
    }

    /// State for plain local HTTP (and tests) with no AWS client attached.
    pub fn local(config: ApiConfig) -> Self {
        Self::new(config, None)
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
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

    /// Attempt the init-time SSM read. Callers decide whether failure is fatal;
    /// local HTTP treats it as a warning so health stays up without floci.
    pub async fn warm(&self) -> Result<&RuntimeConfig, ConfigError> {
        self.runtime_config().await
    }
}

/// Build the application router. Every route lives under `/api/*`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .nest("/api", api_routes())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn api_routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
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
