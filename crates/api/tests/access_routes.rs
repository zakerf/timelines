//! `GET /api/timeline/{id}/access` — the probe the client-side owner chrome
//! asks before revealing anything. It is public, needs no DynamoDB for an
//! anonymous caller, and answers with one boolean and nothing else.

use api::config::{ApiConfig, OidcConfig, RuntimeConfig};
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn state() -> AppState {
    let config = ApiConfig::from_lookup(|_| None).expect("defaults parse");
    let runtime = RuntimeConfig {
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
    };
    AppState::local_with_runtime(config, runtime)
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

#[tokio::test]
async fn anonymous_gets_can_edit_false_without_a_table_read() {
    // No DynamoDB client is attached to this state: the anonymous answer may not
    // need one, and must not fail because of it.
    let response = app(state())
        .oneshot(
            Request::builder()
                .uri("/api/timeline/tl-1/access")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store",
        "a per-viewer answer never enters a shared cache"
    );

    let body = body_text(response).await;
    assert_eq!(body, r#"{"canEdit":false}"#);
    assert!(
        !body.contains("account"),
        "the probe never names or describes the owner"
    );
}

#[tokio::test]
async fn a_stale_session_cookie_still_answers_anonymous() {
    // A cookie with no reachable table resolves to no identity, so the probe
    // degrades to the anonymous answer instead of failing.
    let response = app(state())
        .oneshot(
            Request::builder()
                .uri("/api/timeline/tl-1/access")
                .header(header::COOKIE, "timeline_session=expired-or-unknown")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await, r#"{"canEdit":false}"#);
}

#[tokio::test]
async fn the_probe_is_get_only() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/tl-1/access")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
