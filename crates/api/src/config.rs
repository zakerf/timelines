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
/// Local public base URL of the API. Keycloak owns 8080, so the API is 8081.
pub const DEFAULT_PUBLIC_BASE_URL: &str = "http://localhost:8081";
/// Lifetime of a pending authorization-code state item (PKCE handshake).
pub const AUTH_STATE_TTL_SECONDS: i64 = 600;

/// Environment-derived configuration shared by every request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub table_name: String,
    pub site_bucket: String,
    pub cloudfront_distribution_id_param: String,
    pub oidc_config_param: String,
    pub ip_salt_param: String,
    /// Local-only salt override. Production leaves this unset and reads the
    /// SecureString named by `ip_salt_param` through SSM.
    pub ip_salt: Option<String>,
    pub session_cookie_name: String,
    pub session_ttl_days: u32,
    /// Public origin the browser uses for this API; the OIDC redirect URI is
    /// derived from it, so it must match what the IdP client registers.
    pub public_base_url: String,
    /// KMS key (id, ARN, or alias) used to encrypt refresh tokens. `None`
    /// selects the local fallback (floci runs fine without a KMS key).
    pub kms_key_id: Option<String>,
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
            ip_salt: lookup("IP_SALT").filter(|value| !value.trim().is_empty()),
            session_cookie_name: lookup("SESSION_COOKIE_NAME")
                .unwrap_or_else(|| DEFAULT_SESSION_COOKIE_NAME.to_owned()),
            session_ttl_days: parse_ttl(lookup("SESSION_TTL_DAYS"))?,
            public_base_url: lookup("PUBLIC_BASE_URL")
                .unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.to_owned()),
            kms_key_id: lookup("KMS_KEY_ID").filter(|value| !value.trim().is_empty()),
        };

        if config.session_cookie_name.trim().is_empty() {
            return Err(ConfigError::InvalidEnv {
                name: "SESSION_COOKIE_NAME",
                value: config.session_cookie_name,
            });
        }
        if config.public_base_url.trim().is_empty() {
            return Err(ConfigError::InvalidEnv {
                name: "PUBLIC_BASE_URL",
                value: config.public_base_url,
            });
        }

        Ok(config)
    }

    /// The exact redirect URI registered on the OIDC client.
    pub fn callback_url(&self) -> String {
        format!(
            "{}/api/auth/callback",
            self.public_base_url.trim_end_matches('/')
        )
    }

    /// Whole-second session lifetime, as written on the cookie and TTL item.
    pub fn session_ttl_seconds(&self) -> i64 {
        i64::from(self.session_ttl_days) * 24 * 60 * 60
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

    /// The issuer the ID token must carry.
    ///
    /// The seam deliberately stores only the discovery URL, and every
    /// conformant provider puts the issuer immediately before
    /// `/.well-known/openid-configuration`.
    pub fn expected_issuer(&self) -> Option<&str> {
        self.discovery_url
            .trim_end_matches('/')
            .strip_suffix("/.well-known/openid-configuration")
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
    let oidc = load_oidc_config(config, ssm).await?;
    let distribution_id =
        get_parameter_value(ssm, &config.cloudfront_distribution_id_param).await?;

    Ok(RuntimeConfig {
        distribution_id,
        oidc,
        ip_salt_param: config.ip_salt_param.clone(),
    })
}

/// Resolve only the OIDC seam. Auth needs nothing else from SSM, so a missing
/// distribution-id parameter (or a not-yet-deployed CDN) does not block login.
pub async fn load_oidc_config(
    config: &ApiConfig,
    ssm: &aws_sdk_ssm::Client,
) -> Result<OidcConfig, ConfigError> {
    let oidc_json = get_parameter_value(ssm, &config.oidc_config_param).await?;
    OidcConfig::from_json(&oidc_json).map_err(|error| ConfigError::InvalidOidcJson {
        name: config.oidc_config_param.clone(),
        message: error.to_string(),
    })
}

/// Resolve the out-of-band IP-hash salt (a `SecureString`, PRD §8).
///
/// It is deliberately not part of [`RuntimeConfig`]: only abuse-prevention code
/// needs it, and it must be decrypted.
pub async fn load_ip_salt(
    config: &ApiConfig,
    ssm: &aws_sdk_ssm::Client,
) -> Result<String, ConfigError> {
    let response = ssm
        .get_parameter()
        .name(&config.ip_salt_param)
        .with_decryption(true)
        .send()
        .await
        .map_err(|error| ConfigError::Ssm {
            name: config.ip_salt_param.clone(),
            message: error.to_string(),
        })?;

    response
        .parameter()
        .and_then(|parameter| parameter.value())
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::MissingSsmValue {
            name: config.ip_salt_param.clone(),
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
    MissingDynamoClient,
    MissingS3Client,
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
            Self::MissingDynamoClient => formatter.write_str("no DynamoDB client is configured"),
            Self::MissingS3Client => formatter.write_str("no S3 client is configured"),
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
