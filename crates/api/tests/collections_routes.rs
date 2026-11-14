//! Collection route contracts that need no floci: every mutation is
//! owner-only, and the routes reject the wrong method.

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

fn json_request(method: Method, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request")
}

#[tokio::test]
async fn anonymous_collection_create_is_unauthorized() {
    let response = app(state())
        .oneshot(json_request(
            Method::POST,
            "/api/collections",
            r#"{"title":"Elections 2026"}"#,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_collection_rename_is_unauthorized() {
    let response = app(state())
        .oneshot(json_request(
            Method::PATCH,
            "/api/collections/c1",
            r#"{"title":"Renamed"}"#,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_collection_delete_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/collections/c1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_membership_change_is_unauthorized() {
    let response = app(state())
        .oneshot(json_request(
            Method::PUT,
            "/api/timeline/t1/collection",
            r#"{"collectionId":"c1"}"#,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn collection_create_is_post_only() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/collections")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
