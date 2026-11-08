//! Vote route contracts that need no floci: every mutation and the own-vote
//! read require a signed-in identity.

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
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("direction=up"))
                .expect("request"),
        )
        .await
        .expect("response");

    response.status()
}

#[tokio::test]
async fn anonymous_timeline_vote_is_unauthorized() {
    assert_eq!(
        request(Method::POST, "/api/timeline/t1/vote").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn anonymous_event_vote_is_unauthorized() {
    assert_eq!(
        request(Method::POST, "/api/timeline/t1/event/e1/vote").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn anonymous_own_votes_is_unauthorized() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/votes").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn timeline_vote_is_post_only() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/vote").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
}
