//! Cover route contracts that need no floci: both mutations are owner-only.

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
            client_id: "timeline-site".to_owned(),
            token_endpoint_auth_method: "none".to_owned(),
        },
        ip_salt_param: "/timeline/dev/ip-salt".to_owned(),
    };
    AppState::local_with_runtime(config, runtime)
}

#[tokio::test]
async fn anonymous_cover_upload_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/t1/cover")
                .header(
                    header::CONTENT_TYPE,
                    "multipart/form-data; boundary=----test",
                )
                .body(Body::from("------test--\r\n"))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_cover_removal_is_unauthorized() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/timeline/t1/cover")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_cover_route_rejects_other_methods() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/timeline/t1/cover")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
