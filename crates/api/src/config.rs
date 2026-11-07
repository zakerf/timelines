//! Configuration for the API front door.
//!
//! Two layers keep startup cheap and local-friendly:
//!
//! 1. [`ApiConfig`] parses environment variables (names, defaults, cookie and
//!    session policy). It never talks to AWS.
//! 2. [`RuntimeConfig`] is resolved from SSM the first time auth or a mutation
//!    needs it, so `/api/health` works even when floci is not running.
//!
//! The AWS SDK honours `AWS_ENDPOINT_URL` (and `AWS_ENDPOINT_URL_SSM`) by
//! default, which is the single switch from floci to real AWS.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Local default for `DDB_TABLE`; matches the seed/renderer docs.
pub const DEFAULT_TABLE_NAME: &str = "timeline-data";
/// Local default for `SITE_BUCKET`; the bucket named in `docs/LOCAL_DEV.md`.
pub const DEFAULT_SITE_BUCKET: &str = "timeline-local-site";
/// Local default for the distribution-id parameter written by `modules/cdn`.
pub const DEFAULT_CF_DISTRIBUTION_ID_PARAM: &str = "/timeline/dev/cloudfront/distribution-id";
/// Local default for the OIDC parameter written by `modules/config`.
pub const DEFAULT_OIDC_CONFIG_PARAM: &str = "/timeline/dev/oidc";
/// Local default for the out-of-band IP-hash salt parameter.
pub const DEFAULT_IP_SALT_PARAM: &str = "/timeline/dev/ip-salt";
/// Opaque session cookie name. Tokens never reach the browser.
pub const DEFAULT_SESSION_COOKIE_NAME: &str = "timeline_session";
/// Session lifetime in days; written onto the `SESSION#<id>` item's TTL.
pub const DEFAULT_SESSION_TTL_DAYS: u32 = 30;

/// Environment-derived configuration shared by every request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub table_name: String,
    pub site_bucket: String,
    pub cloudfront_distribution_id_param: String,
    pub oidc_config_param: String,
    pub ip_salt_param: String,
    pub session_cookie_name: String,
    pub session_ttl_days: u32,
}

impl ApiConfig {
    /// Read configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read configuration through an injectable lookup so tests do not mutate
    /// process-global environment variables.
    pub fn from_lookup<F>(mut lookup: F) -> Result<Self, ConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let config = Self {
            table_name: lookup("DDB_TABLE").unwrap_or_else(|| DEFAULT_TABLE_NAME.to_owned()),
            site_bucket: lookup("SITE_BUCKET").unwrap_or_else(|| DEFAULT_SITE_BUCKET.to_owned()),
            cloudfront_distribution_id_param: lookup("CF_DISTRIBUTION_ID_PARAM")
                .unwrap_or_else(|| DEFAULT_CF_DISTRIBUTION_ID_PARAM.to_owned()),
            oidc_config_param: lookup("OIDC_CONFIG_PARAM")
                .unwrap_or_else(|| DEFAULT_OIDC_CONFIG_PARAM.to_owned()),
            ip_salt_param: lookup("IP_SALT_PARAM")
                .unwrap_or_else(|| DEFAULT_IP_SALT_PARAM.to_owned()),
            session_cookie_name: lookup("SESSION_COOKIE_NAME")
                .unwrap_or_else(|| DEFAULT_SESSION_COOKIE_NAME.to_owned()),
            session_ttl_days: parse_ttl(lookup("SESSION_TTL_DAYS"))?,
        };

        if config.session_cookie_name.trim().is_empty() {
            return Err(ConfigError::InvalidEnv {
                name: "SESSION_COOKIE_NAME",
                value: config.session_cookie_name,
            });
        }

        Ok(config)
    }
}

fn parse_ttl(raw: Option<String>) -> Result<u32, ConfigError> {
    match raw {
        None => Ok(DEFAULT_SESSION_TTL_DAYS),
        Some(raw) => match raw.parse::<u32>() {
            Ok(0) => Err(ConfigError::InvalidEnv {
                name: "SESSION_TTL_DAYS",
                value: raw,
            }),
            Ok(days) => Ok(days),
            Err(_) => Err(ConfigError::InvalidEnv {
                name: "SESSION_TTL_DAYS",
                value: raw,
            }),
        },
    }
}

/// The OIDC seam's non-secret configuration (PRD §7).
///
/// `token_endpoint_auth_method` stays a field so a future provider that wants
/// confidential auth is a configuration change, not a rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcConfig {
    pub discovery_url: String,
    pub authorization_url: String,
    pub token_url: String,
    pub jwks_url: String,
    pub client_id: String,
    #[serde(default = "default_token_endpoint_auth_method")]
    pub token_endpoint_auth_method: String,
}

impl OidcConfig {
    /// Parse the JSON stored in the `config` module's SSM `String` parameter.
    pub fn from_json(raw: &str) -> serde_json::Result<Self> {
        serde_json::from_str(raw)
    }
}

fn default_token_endpoint_auth_method() -> String {
    "none".to_owned()
}

/// Values resolved from SSM at init (or lazily on first use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub distribution_id: String,
    pub oidc: OidcConfig,
    pub ip_salt_param: String,
}

/// Fetch the distribution id and OIDC JSON by parameter name.
///
/// The IP-hash salt is only referenced here; it is a `SecureString` created
/// out-of-band and decrypted by the auth layer when it actually needs it.
pub async fn load_runtime_config(
    config: &ApiConfig,
    ssm: &aws_sdk_ssm::Client,
) -> Result<RuntimeConfig, ConfigError> {
    let distribution_id =
        get_parameter_value(ssm, &config.cloudfront_distribution_id_param).await?;
    let oidc_json = get_parameter_value(ssm, &config.oidc_config_param).await?;
    let oidc = OidcConfig::from_json(&oidc_json).map_err(|error| ConfigError::InvalidOidcJson {
        name: config.oidc_config_param.clone(),
        message: error.to_string(),
    })?;

    Ok(RuntimeConfig {
        distribution_id,
        oidc,
        ip_salt_param: config.ip_salt_param.clone(),
    })
}

async fn get_parameter_value(ssm: &aws_sdk_ssm::Client, name: &str) -> Result<String, ConfigError> {
    let response = ssm
        .get_parameter()
        .name(name)
        .with_decryption(false)
        .send()
        .await
        .map_err(|error| ConfigError::Ssm {
            name: name.to_owned(),
            message: error.to_string(),
        })?;

    response
        .parameter()
        .and_then(|parameter| parameter.value())
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::MissingSsmValue {
            name: name.to_owned(),
        })
}

/// Configuration failure that refuses to start the process (or a request that
/// explicitly needs the missing value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidEnv { name: &'static str, value: String },
    MissingSsmClient,
    MissingSsmValue { name: String },
    InvalidOidcJson { name: String, message: String },
    Ssm { name: String, message: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEnv { name, value } => {
                write!(formatter, "invalid value for {name}: {value:?}")
            }
            Self::MissingSsmClient => formatter.write_str("no SSM client is configured"),
            Self::MissingSsmValue { name } => {
                write!(formatter, "SSM parameter {name} has no value")
            }
            Self::InvalidOidcJson { name, message } => {
                write!(
                    formatter,
                    "SSM parameter {name} does not hold valid OIDC JSON: {message}"
                )
            }
            Self::Ssm { name, message } => {
                write!(formatter, "failed to read SSM parameter {name}: {message}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}
