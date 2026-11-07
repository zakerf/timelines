//! Configuration parsing from environment variables and the OIDC SSM JSON.

use api::config::{
    ApiConfig, DEFAULT_SESSION_COOKIE_NAME, DEFAULT_SESSION_TTL_DAYS, DEFAULT_SITE_BUCKET,
    DEFAULT_TABLE_NAME, OidcConfig,
};

#[test]
fn environment_defaults_cover_local_development() {
    let config = ApiConfig::from_lookup(|_| None).expect("defaults parse");

    assert_eq!(config.table_name, DEFAULT_TABLE_NAME);
    assert_eq!(config.site_bucket, DEFAULT_SITE_BUCKET);
    assert_eq!(config.session_cookie_name, DEFAULT_SESSION_COOKIE_NAME);
    assert_eq!(config.session_ttl_days, DEFAULT_SESSION_TTL_DAYS);
    assert_eq!(
        config.cloudfront_distribution_id_param,
        "/timeline/dev/cloudfront/distribution-id"
    );
    assert_eq!(config.oidc_config_param, "/timeline/dev/oidc");
    assert_eq!(config.ip_salt_param, "/timeline/dev/ip-salt");
}

#[test]
fn environment_overrides_every_default() {
    let values = [
        ("DDB_TABLE", "custom-table"),
        ("SITE_BUCKET", "custom-site"),
        ("CF_DISTRIBUTION_ID_PARAM", "/custom/distribution-id"),
        ("OIDC_CONFIG_PARAM", "/custom/oidc"),
        ("IP_SALT_PARAM", "/custom/ip-salt"),
        ("SESSION_COOKIE_NAME", "custom_session"),
        ("SESSION_TTL_DAYS", "7"),
    ];

    let config = ApiConfig::from_lookup(|name| {
        values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    })
    .expect("overrides parse");

    assert_eq!(config.table_name, "custom-table");
    assert_eq!(config.site_bucket, "custom-site");
    assert_eq!(
        config.cloudfront_distribution_id_param,
        "/custom/distribution-id"
    );
    assert_eq!(config.oidc_config_param, "/custom/oidc");
    assert_eq!(config.ip_salt_param, "/custom/ip-salt");
    assert_eq!(config.session_cookie_name, "custom_session");
    assert_eq!(config.session_ttl_days, 7);
}

#[test]
fn invalid_session_ttl_is_rejected() {
    let error =
        ApiConfig::from_lookup(|name| (name == "SESSION_TTL_DAYS").then(|| "soon".to_owned()))
            .expect_err("non-numeric ttl must fail");

    assert!(error.to_string().contains("SESSION_TTL_DAYS"), "{error}");
}

#[test]
fn zero_session_ttl_is_rejected() {
    let error = ApiConfig::from_lookup(|name| (name == "SESSION_TTL_DAYS").then(|| "0".to_owned()))
        .expect_err("zero ttl must fail");

    assert!(error.to_string().contains("SESSION_TTL_DAYS"), "{error}");
}

#[test]
fn oidc_parameter_json_carries_the_four_urls_and_client_id() {
    let raw = r#"{
        "discovery_url": "https://id.example/.well-known/openid-configuration",
        "authorization_url": "https://id.example/oauth2/authorize",
        "token_url": "https://id.example/oauth2/token",
        "jwks_url": "https://id.example/oauth2/jwks",
        "client_id": "timeline-site",
        "token_endpoint_auth_method": "none"
    }"#;

    let oidc = OidcConfig::from_json(raw).expect("oidc json parses");

    assert_eq!(
        oidc.discovery_url,
        "https://id.example/.well-known/openid-configuration"
    );
    assert_eq!(
        oidc.authorization_url,
        "https://id.example/oauth2/authorize"
    );
    assert_eq!(oidc.token_url, "https://id.example/oauth2/token");
    assert_eq!(oidc.jwks_url, "https://id.example/oauth2/jwks");
    assert_eq!(oidc.client_id, "timeline-site");
    assert_eq!(oidc.token_endpoint_auth_method, "none");
}

#[test]
fn oidc_json_defaults_the_auth_method_to_none() {
    let raw = r#"{
        "discovery_url": "https://id.example/.well-known/openid-configuration",
        "authorization_url": "https://id.example/oauth2/authorize",
        "token_url": "https://id.example/oauth2/token",
        "jwks_url": "https://id.example/oauth2/jwks",
        "client_id": "timeline-site"
    }"#;

    let oidc = OidcConfig::from_json(raw).expect("oidc json parses");

    assert_eq!(oidc.token_endpoint_auth_method, "none");
}

#[test]
fn oidc_json_without_a_url_is_rejected() {
    let raw = r#"{
        "authorization_url": "https://id.example/oauth2/authorize",
        "token_url": "https://id.example/oauth2/token",
        "jwks_url": "https://id.example/oauth2/jwks",
        "client_id": "timeline-site"
    }"#;

    let error = OidcConfig::from_json(raw).expect_err("missing discovery url must fail");

    assert!(error.to_string().contains("discovery_url"), "{error}");
}
