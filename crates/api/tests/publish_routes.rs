//! Publish route contracts that need no floci: the mutation is owner-only and
//! the minimal form sends anonymous visitors through login.

use api::config::{ApiConfig, OidcConfig, RuntimeConfig};
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
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

#[tokio::test]
async fn anonymous_publish_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"T","events":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_browser_form_publish_is_still_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9",
                )
                .body(Body::from(
                    "title=T&events=2026-02-04+%7C+T+%7C+https%3A%2F%2Fa.example",
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the browser branch must not invent a session"
    );
}

#[tokio::test]
async fn anonymous_form_redirects_to_login() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .uri("/api/timeline")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/api/auth/login?next=%2Fapi%2Ftimeline"
    );
}

#[tokio::test]
async fn anonymous_append_form_redirects_back_to_itself_after_login() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .uri("/api/timeline/tl-1/events/new")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/api/auth/login?next=%2Fapi%2Ftimeline%2Ftl-1%2Fevents%2Fnew"
    );
}

#[tokio::test]
async fn anonymous_append_form_post_redirects_back_to_itself_after_login() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/tl-1/events")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(
                    "date=2026-02-04&title=T&sources=https%3A%2F%2Fa.example",
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/api/auth/login?next=%2Fapi%2Ftimeline%2Ftl-1%2Fevents%2Fnew"
    );
}

#[tokio::test]
async fn anonymous_append_json_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/tl-1/events")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"date":"2026-02-04","title":"T"}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_import_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"T","description":"D","events":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn import_is_post_only() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/timeline/import")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn publish_is_post_only() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/timeline")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
