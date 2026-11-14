//! The OIDC wire protocol: authorize URL, token exchange, JWKS, and ID-token
//! verification.
//!
//! Everything provider-specific lives in [`crate::config::OidcConfig`]; this
//! module only speaks the standard. The client is public, so the token exchange
//! sends `client_id` and the server-held PKCE verifier and nothing secret.

use std::collections::HashMap;

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use super::error::AuthError;
use crate::config::OidcConfig;

/// Scopes requested at the authorization endpoint.
const SCOPE: &str = "openid profile email";

/// The token endpoint's response. Only the ID token is required; the refresh
/// token is what gets encrypted into the account row.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub id_token: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// The ID-token claims the seam actually consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    pub iss: String,
    pub aud: Audience,
    pub exp: i64,
    #[serde(default)]
    pub iat: Option<i64>,
    /// Cognito emits `token_use`; Keycloak does not. When present it must be
    /// the ID token, never an access token.
    #[serde(default)]
    pub token_use: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// `aud` is a string in the spec but providers emit either shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

impl Audience {
    fn contains(&self, audience: &str) -> bool {
        match self {
            Self::Single(value) => value == audience,
            Self::Multiple(values) => values.iter().any(|value| value == audience),
        }
    }
}

impl IdTokenClaims {
    /// Best available human label, never empty.
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.preferred_username
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                self.email
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or(&self.sub)
            .to_owned()
    }
}

/// Why an ID token was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdTokenError {
    Malformed(String),
    UnsupportedAlgorithm(String),
    UnknownKey(String),
    InvalidSignature(String),
    Expired,
    InvalidIssuer { expected: String, actual: String },
    InvalidAudience { expected: String },
    WrongTokenUse(String),
    MissingSubject,
}

impl std::fmt::Display for IdTokenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(message) => write!(formatter, "malformed ID token: {message}"),
            Self::UnsupportedAlgorithm(algorithm) => {
                write!(formatter, "unsupported ID-token algorithm {algorithm:?}")
            }
            Self::UnknownKey(kid) => write!(formatter, "no JWKS key matches kid {kid:?}"),
            Self::InvalidSignature(message) => {
                write!(formatter, "invalid ID-token signature: {message}")
            }
            Self::Expired => formatter.write_str("ID token has expired"),
            Self::InvalidIssuer { expected, actual } => {
                write!(formatter, "issuer {actual:?} does not match {expected:?}")
            }
            Self::InvalidAudience { expected } => {
                write!(formatter, "audience does not contain {expected:?}")
            }
            Self::WrongTokenUse(value) => {
                write!(formatter, "token_use is {value:?}, not \"id\"")
            }
            Self::MissingSubject => formatter.write_str("ID token has an empty subject"),
        }
    }
}

impl std::error::Error for IdTokenError {}

/// Build the redirect to the provider's authorization endpoint.
pub fn build_authorize_url(
    oidc: &OidcConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String, AuthError> {
    let mut url = Url::parse(&oidc.authorization_url)
        .map_err(|error| AuthError::internal(format!("invalid authorization_url: {error}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

/// Fetch the provider's JSON Web Key Set.
pub async fn fetch_jwks(http: &Client, jwks_url: &str) -> Result<JwkSet, AuthError> {
    let response = http
        .get(jwks_url)
        .send()
        .await
        .map_err(|error| AuthError::upstream(format!("JWKS request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AuthError::upstream(format!(
            "JWKS endpoint returned {}",
            response.status()
        )));
    }
    response
        .json::<JwkSet>()
        .await
        .map_err(|error| AuthError::upstream(format!("invalid JWKS document: {error}")))
}

/// Exchange an authorization code for tokens using the server-held verifier.
pub async fn exchange_code(
    http: &Client,
    oidc: &OidcConfig,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, AuthError> {
    if oidc.token_endpoint_auth_method != "none" {
        return Err(AuthError::internal(format!(
            "token_endpoint_auth_method {:?} is not configured (the seam is a public client)",
            oidc.token_endpoint_auth_method
        )));
    }

    #[derive(Serialize)]
    struct Form<'a> {
        grant_type: &'a str,
        code: &'a str,
        redirect_uri: &'a str,
        client_id: &'a str,
        code_verifier: &'a str,
    }

    let response = http
        .post(&oidc.token_url)
        .form(&Form {
            grant_type: "authorization_code",
            code,
            redirect_uri,
            client_id: &oidc.client_id,
            code_verifier,
        })
        .send()
        .await
        .map_err(|error| AuthError::upstream(format!("token request failed: {error}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::upstream(format!(
            "token endpoint returned {status}: {}",
            truncate(&body)
        )));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(|error| AuthError::upstream(format!("invalid token response: {error}")))
}

/// Ask the provider to revoke a refresh token. Logout is best-effort: the
/// session item deletion is the real revocation, so a provider error is logged
/// by the caller and never blocks the redirect.
pub async fn revoke_refresh_token(
    http: &Client,
    revocation_url: &str,
    oidc: &OidcConfig,
    refresh_token: &str,
) -> Result<(), AuthError> {
    let form = HashMap::from([
        ("token", refresh_token),
        ("token_type_hint", "refresh_token"),
        ("client_id", oidc.client_id.as_str()),
    ]);

    let response = http
        .post(revocation_url)
        .form(&form)
        .send()
        .await
        .map_err(|error| AuthError::upstream(format!("revocation request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AuthError::upstream(format!(
            "revocation endpoint returned {}",
            response.status()
        )));
    }
    Ok(())
}

/// Read the discovery document for the revocation endpoint. Returns `None`
/// when the provider does not advertise one.
pub async fn fetch_revocation_endpoint(
    http: &Client,
    discovery_url: &str,
) -> Result<Option<String>, AuthError> {
    #[derive(Deserialize)]
    struct Discovery {
        #[serde(default)]
        revocation_endpoint: Option<String>,
    }

    let response = http
        .get(discovery_url)
        .send()
        .await
        .map_err(|error| AuthError::upstream(format!("discovery request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AuthError::upstream(format!(
            "discovery endpoint returned {}",
            response.status()
        )));
    }
    let discovery = response
        .json::<Discovery>()
        .await
        .map_err(|error| AuthError::upstream(format!("invalid discovery document: {error}")))?;
    Ok(discovery.revocation_endpoint)
}

/// Verify an ID token against the provider's JWKS and the seam's expectations.
///
/// RS256 only; `exp` is enforced by the JWT validator, while `iss`, `aud`, and
/// `token_use` are checked here so both string and array audiences work.
pub fn verify_id_token(
    token: &str,
    jwks: &JwkSet,
    expected_issuer: &str,
    expected_audience: &str,
) -> Result<IdTokenClaims, IdTokenError> {
    let header =
        decode_header(token).map_err(|error| IdTokenError::Malformed(error.to_string()))?;
    if header.alg != Algorithm::RS256 {
        return Err(IdTokenError::UnsupportedAlgorithm(format!(
            "{:?}",
            header.alg
        )));
    }

    let jwk = match header.kid.as_deref() {
        Some(kid) => jwks
            .find(kid)
            .ok_or_else(|| IdTokenError::UnknownKey(kid.to_owned()))?,
        None if jwks.keys.len() == 1 => &jwks.keys[0],
        None => return Err(IdTokenError::UnknownKey(String::new())),
    };
    let key = DecodingKey::from_jwk(jwk)
        .map_err(|error| IdTokenError::InvalidSignature(error.to_string()))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = true;
    validation.validate_aud = false;
    validation.leeway = 0;
    validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    let claims = decode::<IdTokenClaims>(token, &key, &validation)
        .map_err(|error| match error.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => IdTokenError::Expired,
            _ => IdTokenError::InvalidSignature(error.to_string()),
        })?
        .claims;

    if normalize_issuer(&claims.iss) != normalize_issuer(expected_issuer) {
        return Err(IdTokenError::InvalidIssuer {
            expected: expected_issuer.to_owned(),
            actual: claims.iss,
        });
    }
    if !claims.aud.contains(expected_audience) {
        return Err(IdTokenError::InvalidAudience {
            expected: expected_audience.to_owned(),
        });
    }
    if let Some(token_use) = &claims.token_use
        && token_use != "id"
    {
        return Err(IdTokenError::WrongTokenUse(token_use.clone()));
    }
    if claims.sub.trim().is_empty() {
        return Err(IdTokenError::MissingSubject);
    }

    Ok(claims)
}

fn normalize_issuer(issuer: &str) -> &str {
    issuer.trim_end_matches('/')
}

fn truncate(value: &str) -> String {
    const LIMIT: usize = 240;
    let mut truncated: String = value.chars().take(LIMIT).collect();
    if truncated.len() < value.len() {
        truncated.push_str("...");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    const ISSUER: &str = "http://localhost:8080/realms/timeline";
    const AUDIENCE: &str = "timelines";
    const KID: &str = "test-key";

    const PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC2gBRw4R0FfVO6
fMaSp+t4FyLBIBYojv3aMmivnR7mOTraRlbfqOvnTMSs/pQXXPoSZhsvl5iw5lXj
DymN4F38rppfn94jW9Ns4mBT8Ws5n5RIYQH/4FVWH6B2jci6n4DYvY0P08Tp+Vnk
M9W7jkG+Hsy7PIaUHZX1yAVQ1b8DEl1YR1++NpS2JofurAiJSOv35fU2x/fjJk62
amBt8oiMF96Oz6JnMXH9Vkwm/4etpSkK/0hPVgGu0X6VnP15RpBzt/4X3XvwiffA
hewq8IlPbaoGMWCYHaUF9BKD33NBmDr34lASbn/TrNnT3D34IIWZ9cl4+f6Nr6kF
zX/kJwAJAgMBAAECggEAIsObITWAlb8d5/REz0L1c9bUY/18uxUh/iKEFo/Thv93
2oRzn/9JkLHq9Rk5lIm9iSuyGgrCp0/5JFuLCt+mh6+q8HQ/mHSxfZhRySu/xSZE
1NTF0LwnBVTY+Dxh9ErV/HN/H3izle5N1U6adDPRCipLzY7mA+ju2/bSLGfLDwZq
B5UksKZaeJ2mXMir5u5StllVrRTQY4wNbM8Er40M29KAwWqO8oiP+q2QflgwSqGm
iuELOs4E+OBGxB+bGbObGW1TzE/F0WK8r6klOZAv9icuwGhbmFN6ZicujZvwp7mT
1kwHubY8nZghwkFnQNjYvz66VEnNC/IXwv1E8pvfQQKBgQDmBc8FxzXU3NaBSvGh
s2XpfQUDT8s8K+lBwcbxFMKEoTUH8wBrYPiScnB3Da/2ZNWPY2lxLvImtPrsjg5j
S6HtHuJIsRsI5mIOTEicp4+0141upwtctGV+TwvYAoCrfTtlYR5SwCgtc0qcQjSX
zwkDixIx7GszhK2L+3t9/SVKdQKBgQDLHFnAIZfwgsYUsq0ufhMK6i+GPGf95wzM
y17p105wLDDqDNqb5IUa3LohTUz4fv1YmRNLmujnjGQnJIuVT9/C+MtELF3X0xsS
09fT2JLAfPqgG1UD9SCtXbarxAFjtBNuUnEFUvkBqtjcpgOmcUWSyhSxUzg6t6g4
VRsiTgNkxQKBgQCW2SpaLLmV3jniUqbCF6d3ByEZeePbkySGljEEe0vRtPLVQrx8
rvx09g3hVZ2XJauUPGqy6kmtrrR8G/E9lyUbH0zgxRzoqkWWANsBuTWaMF41OeFO
q5N/0REAWSvOyvL/2kALnq76l4hPj290Ozc7FqIZEWWARLuXeJYVGsWw3QKBgQC6
b+qeZveuCIxEIYLAI2/dW/uFr1sRgA2Kyr7zzPOuGZJItMXRxSC8+JFN0kPt8bsh
oVlJ1AcnzAF+wJfz1Z6ivHbwu/zkCF5YmOYLCkgislXns08FU/PLzzLHViKkX35a
jQg7h98TvbxURXsiQ+VGCYKXBrmdhEnAc7Z1dcC9tQKBgFbvPyfE/5RclZO3eHee
ud46NIlZUD+T9GMIwiOr2SWuJloLAKIokAZAmIytAn0qQeRdineQ1gz8jhBvYToQ
SACjHwL+gYyYR2784r1zgDGG/MXIflW5ZoeLbLFX+lMm8bO7RMeVzBNS/5C7sJAi
fjhclc8bZHYv0r+YXagHQG/V
-----END PRIVATE KEY-----"#;

    const OTHER_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCe+XNCo71rgvA8
NCld0mmo/ZXRdOQ91bwOeZdwny0JMmEm+ynEC3GsZO6uiv9cBY7xjrK14avp8XHc
VgsilF7CMo9HEWp1Xe/fbTSbA9ZP2yH7pedTg804Bqy3aC0zvkRp6EVqF8ahVXK8
QcgUNkLvqQpJV+mAYCMBd1wCeJmxU1KLZFCAFJe4o443TGYFp1mxaUeLTSdrPAio
x+I894Cn+osGG+t28snURHVBqniUIenrsuvw5gDZXlYAzS4vuW0ikRA+wrUN9y9J
r7JniWDOEAVQ8NG1MxIgPTkuslQe2RKlETs0xNmTj4YN1uit2edqyDizdSc0U00J
HwcbAdi1AgMBAAECggEAJV8x89wHRxNwg06l3Nr+oM4AXs2SbGDnBSYbbkvVLADO
avoHbwFbtzHBFIIzlr/twyaPJ4fGQMhUQn5tvrLPdeqp2QSkmLO9FLKY98oWow46
NmzhbTYiU8Xp63HPkobrkVQboV22ga3TO6NhNDMvWCTOeMB0cYvoHsue8yvDwbYc
XzU9+iBiq1n61lE1gNJ/KKP2mNPe3Rqu1GtnbbXVkRoxyXDHupbEpTBHAPrTYAOo
/5Vk6GxWk9nYkfMjNYfAHjkdkl/PUCtKcMmyw26ptkM86BAWLk04hvAtWPz8WGEb
BQBKVK7lXSiwhRTzIkHaCAIXBk5waJclIjzeTe128QKBgQDQPZBUqNsAhHQkn9x8
4oLLb/RRfCuM/H5rGBWTMA/Uk8qtMAKTya4sy63LfxlyZfA52xbMkhKJJbbQXM+j
7yKD/R4VCyQsjajRjyyXt+lLAiCuZfJ3HM2jErBruCGIfEw5YiPQYwSOuxi3R5cx
EZr965QaQqeo2beKh6uI7QbPSQKBgQDDb1MjLICe5K6GRlcY3gTLOAZqt78Qpm39
sG4NIaNIbnv4qURVopvSDHyBeknEbIE/JxOzzJKybd3vAiKgCJfQWMgRmcU0ZdfD
19ZzeTD8WZLZm+Q4ebc7DsSWWAtLRpq3eoVG6Wz6JbRRimnz0xi7MH1noy8i1Jz6
NKZ4n/TCDQKBgQCAZpWKPhvbs9BRjOjNcHWprzx8yg2of0AMFOuouCfqsb51D3dn
XSIyqNxe793m9ggsBHWIakFXo5OX4sJ9mMQ1kmtsGlSRzD9eB+Q9IL+O6p7wKhhZ
bb82YB28LesvV0VA1My0f3md8KuKx2/oZ6ykCRBnKNZsfOJ5XrJcWZqrUQKBgDxg
rUelnQr5zW1QV+In3UVHRrw7jjy/niWNF0LZTI+5CnXVHXZzaT5oeyKpPBcCCqAP
G4glwIB9bt5DQ507amOEujKt1f4NJK6thORh4Z9ksPinr8c+2zvUoSltg/e6lVvv
7zLsNK3HbkDuyTL52Z8jKOsWwB47n0+xamjDyiMVAoGBAIP7PCmuxWBgFcr4eu/n
nHRuPI5fZCndgGefkkX0k1iPsfhkAjQseBiJYvpj8iNB/50iWsmixMwCN0aISlHZ
5aUegd6/8R5ZrnCLBVsilRnlTS9Q/YP8HYp/AsL1pndEi7lVO1fPrEqIosNjLXg2
vnSBo0qr2XD3h4xOrPq3gMih
-----END PRIVATE KEY-----"#;

    fn jwks() -> JwkSet {
        // Modulus/exponent of PRIVATE_KEY (test-only key).
        serde_json::from_value(json!({
            "keys": [{
                "kty": "RSA",
                "kid": KID,
                "alg": "RS256",
                "use": "sig",
                "n": "toAUcOEdBX1TunzGkqfreBciwSAWKI792jJor50e5jk62kZW36jr50zErP6UF1z6EmYbL5eYsOZV4w8pjeBd_K6aX5_eI1vTbOJgU_FrOZ-USGEB_-BVVh-gdo3Iup-A2L2ND9PE6flZ5DPVu45Bvh7MuzyGlB2V9cgFUNW_AxJdWEdfvjaUtiaH7qwIiUjr9-X1Nsf34yZOtmpgbfKIjBfejs-iZzFx_VZMJv-HraUpCv9IT1YBrtF-lZz9eUaQc7f-F9178In3wIXsKvCJT22qBjFgmB2lBfQSg99zQZg69-JQEm5_06zZ09w9-CCFmfXJePn-ja-pBc1_5CcACQ",
                "e": "AQAB"
            }]
        }))
        .expect("test JWKS is valid")
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn claims(issuer: &str, audience: serde_json::Value, exp: i64) -> serde_json::Value {
        json!({
            "sub": "user-123",
            "iss": issuer,
            "aud": audience,
            "exp": exp,
            "iat": now(),
            "name": "Ada Lovelace",
            "preferred_username": "ada",
            "email": "ada@example.com"
        })
    }

    fn sign_with(key_pem: &str, claims: &serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_owned());
        let key = EncodingKey::from_rsa_pem(key_pem.as_bytes()).expect("test key parses");
        encode(&header, claims, &key).expect("test token encodes")
    }

    fn sign(claims: &serde_json::Value) -> String {
        sign_with(PRIVATE_KEY, claims)
    }

    #[test]
    fn accepts_a_valid_id_token() {
        let token = sign(&claims(ISSUER, json!(AUDIENCE), now() + 300));

        let verified = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect("valid token");

        assert_eq!(verified.sub, "user-123");
        assert_eq!(verified.display_name(), "Ada Lovelace");
    }

    #[test]
    fn accepts_an_array_audience_and_a_trailing_slash_on_the_issuer() {
        let token = sign(&claims(
            &format!("{ISSUER}/"),
            json!(["something-else", AUDIENCE]),
            now() + 300,
        ));

        let verified =
            verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect("array audience is valid");

        assert_eq!(verified.sub, "user-123");
    }

    #[test]
    fn rejects_a_bad_issuer() {
        let token = sign(&claims(
            "http://evil.example/realms/timeline",
            json!(AUDIENCE),
            now() + 300,
        ));

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("bad issuer");

        assert!(
            matches!(error, IdTokenError::InvalidIssuer { .. }),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_bad_audience() {
        let token = sign(&claims(ISSUER, json!("another-client"), now() + 300));

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("bad audience");

        assert!(
            matches!(error, IdTokenError::InvalidAudience { .. }),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_expired_token() {
        let token = sign(&claims(ISSUER, json!(AUDIENCE), now() - 120));

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("expired token");

        assert!(matches!(error, IdTokenError::Expired), "{error}");
    }

    #[test]
    fn rejects_a_token_signed_by_the_wrong_key() {
        let token = sign_with(
            OTHER_PRIVATE_KEY,
            &claims(ISSUER, json!(AUDIENCE), now() + 300),
        );

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("wrong key");

        assert!(
            matches!(error, IdTokenError::InvalidSignature(_)),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_access_token_masquerading_as_an_id_token() {
        let mut claims = claims(ISSUER, json!(AUDIENCE), now() + 300);
        claims["token_use"] = json!("access");
        let token = sign(&claims);

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("token_use");

        assert!(matches!(error, IdTokenError::WrongTokenUse(_)), "{error}");
    }

    #[test]
    fn rejects_an_unknown_key_id() {
        let token = {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some("unknown".to_owned());
            let key = EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes()).expect("test key");
            encode(&header, &claims(ISSUER, json!(AUDIENCE), now() + 300), &key).expect("token")
        };

        let error = verify_id_token(&token, &jwks(), ISSUER, AUDIENCE).expect_err("unknown kid");

        assert!(matches!(error, IdTokenError::UnknownKey(_)), "{error}");
    }

    #[test]
    fn builds_the_authorization_redirect_with_pkce() {
        let oidc = OidcConfig {
            discovery_url: format!("{ISSUER}/.well-known/openid-configuration"),
            authorization_url: format!("{ISSUER}/protocol/openid-connect/auth"),
            token_url: format!("{ISSUER}/protocol/openid-connect/token"),
            jwks_url: format!("{ISSUER}/protocol/openid-connect/certs"),
            client_id: AUDIENCE.to_owned(),
            token_endpoint_auth_method: "none".to_owned(),
        };

        let url = build_authorize_url(
            &oidc,
            "http://localhost:8081/api/auth/callback",
            "state-1",
            "challenge-1",
        )
        .expect("authorize url");
        let parsed = Url::parse(&url).expect("url");

        assert_eq!(
            parsed.path(),
            "/realms/timeline/protocol/openid-connect/auth"
        );
        let pairs: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["client_id"], AUDIENCE);
        assert_eq!(
            pairs["redirect_uri"],
            "http://localhost:8081/api/auth/callback"
        );
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["state"], "state-1");
        assert_eq!(pairs["code_challenge"], "challenge-1");
    }

    #[test]
    fn derives_the_issuer_from_the_discovery_url() {
        let oidc = OidcConfig {
            discovery_url: format!("{ISSUER}/.well-known/openid-configuration"),
            authorization_url: String::new(),
            token_url: String::new(),
            jwks_url: String::new(),
            client_id: String::new(),
            token_endpoint_auth_method: "none".to_owned(),
        };

        assert_eq!(oidc.expected_issuer(), Some(ISSUER));
    }
}
