//! Route-level auth contracts that need no floci: anonymous identity, callback
//! refusals, and cookie attributes on logout.

use api::config::{ApiConfig, OidcConfig, RuntimeConfig};
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn runtime() -> RuntimeConfig {
    RuntimeConfig {
        distribution_id: "local-distribution".to_owned(),
        oidc: OidcConfig {
            discovery_url: "http://localhost:8080/realms/timeline/.well-known/openid-configuration"
                .to_owned(),
            authorization_url: "http://localhost:8080/realms/timeline/protocol/openid-connect/auth"
                .to_owned(),
            token_url: "http://localhost:8080/realms/timeline/protocol/openid-connect/token"
                .to_owned(),
            jwks_url: "http://localhost:8080/realms/timeline/protocol/openid-connect/certs"
                .to_owned(),
            client_id: "timelines".to_owned(),
            token_endpoint_auth_method: "none".to_owned(),
        },
        ip_salt_param: "/timeline/dev/ip-salt".to_owned(),
    }
}

fn local_state() -> AppState {
    let config = ApiConfig::from_lookup(|_| None).expect("defaults parse");
    AppState::local_with_runtime(config, runtime())
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn me_is_anonymous_without_a_cookie() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["authenticated"], false);
    assert!(json.get("account").is_none());
}

#[tokio::test]
async fn me_treats_an_unknown_session_as_anonymous() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header(header::COOKIE, "timeline_session=not-a-real-session")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["authenticated"], false);
}

#[tokio::test]
async fn callback_without_a_code_is_a_bad_request() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?state=abc")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_without_state_is_a_bad_request() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?code=abc")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_requires_the_browser_bound_state_cookie() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?code=abc&state=xyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_rejects_a_mismatched_state_cookie() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?code=abc&state=xyz")
                .header(header::COOKIE, "timeline_oauth_state=other")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_surfaces_a_provider_error() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?error=access_denied&error_description=user+cancelled")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logout_clears_the_cookie_with_the_required_attributes() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/logout")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/");

    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .expect("cookie header");
    assert!(cookie.starts_with("timeline_session=;"), "{cookie}");
    assert!(cookie.contains("Max-Age=0"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("Secure"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
}

#[tokio::test]
async fn logout_is_post_only() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/auth/logout")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
