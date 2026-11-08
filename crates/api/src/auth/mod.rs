//! The OIDC auth seam: login, callback, logout, and the request identity.
//!
//! Public client, authorization-code + PKCE, no client secret. The browser
//! only ever holds an opaque session cookie; tokens stay server-side (PRD §7).

pub mod cipher;
pub mod cookie;
pub mod error;
pub mod oidc;
pub mod pkce;
pub mod store;

use std::convert::Infallible;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use data::Timestamp;
use data::model::{Account, AuthState};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::config::AUTH_STATE_TTL_SECONDS;
use error::AuthError;

/// Mount the auth endpoints under `/api/auth/*`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(callback))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

/// A signed-in identity for downstream handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub account_id: String,
    pub display_name: String,
    pub session_id: String,
}

/// Extractor that yields `None` for anonymous requests and never rejects.
#[derive(Debug, Clone)]
pub struct OptionalIdentity(pub Option<Identity>);

impl FromRequestParts<AppState> for OptionalIdentity {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(load_identity(&parts.headers, state).await))
    }
}

/// Extractor for owner-only mutations. Anonymous requests are refused with a
/// `401` before the handler runs.
impl FromRequestParts<AppState> for Identity {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        load_identity(&parts.headers, state)
            .await
            .ok_or_else(|| AuthError::unauthorized("sign in to continue"))
    }
}

async fn load_identity(headers: &HeaderMap, state: &AppState) -> Option<Identity> {
    let session_id = cookie::value_from_headers(headers, &state.config().session_cookie_name)?;
    let ddb = state.dynamodb_opt()?;
    let table = &state.config().table_name;

    let session = store::get_session(ddb, table, &session_id).await.ok()??;
    if session.expires_at <= data::timestamp::now_epoch_seconds() {
        return None;
    }
    let account = store::get_account(ddb, table, &session.account_id)
        .await
        .ok()??;

    Some(Identity {
        account_id: account.sub,
        display_name: account.display_name,
        session_id: session.id,
    })
}

/// Start a login: store the PKCE verifier server-side, bind `state` to this
/// browser with a short-lived cookie, and redirect to the IdP.
async fn login(State(state): State<AppState>) -> Result<Response, AuthError> {
    let oidc = state.oidc_config().await?.clone();
    let ddb = state.dynamodb()?;
    let config = state.config();

    let code_verifier = pkce::generate_verifier();
    let code_challenge = pkce::generate_challenge(&code_verifier);
    let state_token = pkce::generate_token();
    let redirect_uri = config.callback_url();

    let handshake = AuthState {
        state: state_token.clone(),
        code_verifier,
        redirect_uri: redirect_uri.clone(),
        created_at: Timestamp::now(),
        expires_at: data::timestamp::now_epoch_seconds() + AUTH_STATE_TTL_SECONDS,
    };
    store::put_auth_state(ddb, &config.table_name, &handshake).await?;

    let url = oidc::build_authorize_url(&oidc, &redirect_uri, &state_token, &code_challenge)?;
    let state_cookie = cookie::state_cookie(&state_token, AUTH_STATE_TTL_SECONDS);
    redirect_with_cookies(&url, &[&state_cookie])
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Finish a login: verify state, exchange the code, verify the ID token,
/// upsert the account, and set the session cookie.
async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Result<Response, AuthError> {
    if let Some(error) = query.error {
        return Err(AuthError::bad_request(format!(
            "provider returned {error}: {}",
            query.error_description.unwrap_or_default()
        )));
    }

    let code = query
        .code
        .ok_or_else(|| AuthError::bad_request("missing code"))?;
    let state_id = query
        .state
        .ok_or_else(|| AuthError::bad_request("missing state"))?;

    // The browser that started the login must be the one finishing it.
    let cookie_state = cookie::value_from_headers(&headers, cookie::OAUTH_STATE_COOKIE_NAME)
        .ok_or_else(|| AuthError::bad_request("missing state cookie"))?;
    if cookie_state != state_id {
        return Err(AuthError::bad_request("state cookie does not match state"));
    }

    let ddb = state.dynamodb()?;
    let table = &state.config().table_name;

    let handshake = store::take_auth_state(ddb, table, &state_id)
        .await?
        .ok_or_else(|| AuthError::bad_request("unknown or expired state"))?;

    let oidc = state.oidc_config().await?;
    let tokens = oidc::exchange_code(
        state.http(),
        oidc,
        &code,
        &handshake.code_verifier,
        &handshake.redirect_uri,
    )
    .await?;

    let jwks = oidc::fetch_jwks(state.http(), &oidc.jwks_url).await?;
    let issuer = oidc
        .expected_issuer()
        .ok_or_else(|| AuthError::internal("cannot derive the issuer from discovery_url"))?;
    let claims = oidc::verify_id_token(&tokens.id_token, &jwks, issuer, &oidc.client_id)
        .map_err(|error| AuthError::bad_request(format!("ID token rejected: {error}")))?;

    let refresh_token_ciphertext = match tokens.refresh_token.as_deref() {
        Some(token) => Some(state.cipher().encrypt(token).await?),
        None => None,
    };

    let now = Timestamp::now();
    let account = Account {
        sub: claims.sub.clone(),
        display_name: claims.display_name(),
        created_at: now.clone(),
        last_seen_at: now,
        refresh_token_ciphertext,
    };
    store::upsert_account(ddb, table, &account).await?;

    let config = state.config();
    let session = store::new_session(&account.sub, config.session_ttl_seconds());
    store::create_session(ddb, table, &session).await?;

    let cookie = cookie::session_cookie(
        &config.session_cookie_name,
        &session.id,
        config.session_ttl_seconds(),
    );
    let cleared_state = cookie::clear_state_cookie();
    redirect_with_cookies("/", &[&cookie, &cleared_state])
}

/// End the session: delete the SESSION item, best-effort revoke the refresh
/// token at the provider, and clear the cookie.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AuthError> {
    let cookie_name = state.config().session_cookie_name.clone();

    if let Some(session_id) = cookie::value_from_headers(&headers, &cookie_name)
        && let Some(ddb) = state.dynamodb_opt()
    {
        let table = &state.config().table_name;
        if let Ok(Some(session)) = store::get_session(ddb, table, &session_id).await {
            if let Ok(Some(ciphertext)) =
                store::get_refresh_token_ciphertext(ddb, table, &session.account_id).await
            {
                revoke_best_effort(&state, &ciphertext).await;
            }
            store::delete_session(ddb, table, &session_id).await?;
        }
    }

    let clear = cookie::clear_cookie(&cookie_name);
    redirect_with_cookies("/", &[&clear])
}

async fn revoke_best_effort(state: &AppState, ciphertext: &str) {
    let Ok(oidc) = state.oidc_config().await else {
        tracing::warn!("cannot revoke refresh token: OIDC configuration unavailable");
        return;
    };
    let revocation_url =
        match oidc::fetch_revocation_endpoint(state.http(), &oidc.discovery_url).await {
            Ok(Some(url)) => url,
            Ok(None) => {
                tracing::warn!("provider does not advertise a revocation endpoint");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "could not discover the revocation endpoint");
                return;
            }
        };
    let refresh_token = match state.cipher().decrypt(ciphertext).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "could not decrypt the refresh token");
            return;
        }
    };
    if let Err(error) =
        oidc::revoke_refresh_token(state.http(), &revocation_url, oidc, &refresh_token).await
    {
        tracing::warn!(%error, "refresh-token revocation failed");
    }
}

#[derive(Debug, Serialize)]
struct MeResponse {
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<MeAccount>,
}

#[derive(Debug, Serialize)]
struct MeAccount {
    id: String,
    display_name: String,
}

/// The signed-in identity, anonymous when no valid session cookie is present.
async fn me(OptionalIdentity(identity): OptionalIdentity) -> Json<MeResponse> {
    Json(match identity {
        Some(identity) => MeResponse {
            authenticated: true,
            account: Some(MeAccount {
                id: identity.account_id,
                display_name: identity.display_name,
            }),
        },
        None => MeResponse {
            authenticated: false,
            account: None,
        },
    })
}

fn redirect_with_cookies(location: &str, set_cookies: &[&str]) -> Result<Response, AuthError> {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(location)
            .map_err(|error| AuthError::internal(format!("invalid redirect: {error}")))?,
    );
    for cookie in set_cookies {
        response.headers_mut().append(
            SET_COOKIE,
            HeaderValue::from_str(cookie)
                .map_err(|error| AuthError::internal(format!("invalid cookie: {error}")))?,
        );
    }
    Ok(response)
}
