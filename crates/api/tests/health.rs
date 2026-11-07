//! The `/api/health` seam: the front door's liveness contract.

use api::config::ApiConfig;
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn local_state() -> AppState {
    AppState::local(ApiConfig::from_lookup(|_| None).expect("defaults parse"))
}

#[tokio::test]
async fn health_returns_200_with_status_ok() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn health_is_only_mounted_under_the_api_prefix() {
    let response = app(local_state())
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
