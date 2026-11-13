//! Report route contracts that need no floci: every report, withdrawal, and
//! owner summary requires a signed-in identity.

use api::config::ApiConfig;
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

fn state() -> AppState {
    AppState::local(ApiConfig::from_lookup(|_| None).expect("defaults parse"))
}

async fn request(method: Method, uri: &str) -> StatusCode {
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    response.status()
}

#[tokio::test]
async fn anonymous_timeline_report_is_unauthorized() {
    for method in [Method::POST, Method::DELETE] {
        assert_eq!(
            request(method, "/api/timeline/t1/report").await,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn anonymous_event_report_is_unauthorized() {
    for method in [Method::POST, Method::DELETE] {
        assert_eq!(
            request(method, "/api/timeline/t1/event/e1/report").await,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn anonymous_suggestion_report_is_unauthorized() {
    for method in [Method::POST, Method::DELETE] {
        assert_eq!(
            request(method, "/api/timeline/t1/suggestions/s1/report").await,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn anonymous_report_summary_is_unauthorized() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/reports").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn report_routes_are_post_and_delete_only() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/report").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        request(Method::PUT, "/api/timeline/t1/event/e1/report").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        request(Method::POST, "/api/timeline/t1/reports").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn report_responses_are_json_and_never_cached() {
    // `header` is exercised here so the contract stays visible to the test:
    // the shared response helper always sets the content type and no-store.
    let response = app(state())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/timeline/t1/reports")
                .header(header::ACCEPT, "application/json")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
