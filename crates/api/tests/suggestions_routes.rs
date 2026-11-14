//! Suggestion route contracts that need no floci: submission, owner review,
//! and the owner queue are all authenticated.

use api::config::{ApiConfig, OidcConfig, RuntimeConfig};
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
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

async fn status(request: Request<Body>) -> StatusCode {
    app(state())
        .oneshot(request)
        .await
        .expect("response")
        .status()
}

#[tokio::test]
async fn anonymous_submission_is_unauthorized() {
    assert_eq!(
        status(
            Request::builder()
                .method(Method::POST)
                .uri("/api/timeline/t1/suggestions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"APPEND","payload":{}}"#))
                .expect("request"),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn anonymous_review_is_unauthorized() {
    for action in ["approve", "reject"] {
        assert_eq!(
            status(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/timeline/t1/suggestions/s1/{action}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn the_owner_queue_is_authenticated() {
    assert_eq!(
        status(
            Request::builder()
                .uri("/api/suggestions/queue")
                .body(Body::empty())
                .expect("request"),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn suggestion_mutations_are_post_only() {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/timeline/t1/suggestions/s1/approve")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
