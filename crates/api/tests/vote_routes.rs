//! Vote route contracts that need no floci: every mutation and the own-vote
//! read require a signed-in identity, and a refusal is a plain, `no-store`
//! response the browser script can map to a visible message.
//!
//! The browser half of these refusals is `crates/renderer/assets/app.js`
//! (`401` -> "Sign in to vote", `403` -> self-vote, `429` + `Retry-After` ->
//! rate limit); these tests pin the statuses and bodies it reads.

use api::config::ApiConfig;
use api::{AppState, app};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn state() -> AppState {
    AppState::local(ApiConfig::from_lookup(|_| None).expect("defaults parse"))
}

async fn send(method: Method, uri: &str, content_type: &str, body: &str) -> Response {
    app(state())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body.to_owned()))
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn request(method: Method, uri: &str) -> StatusCode {
    send(
        method,
        uri,
        "application/x-www-form-urlencoded",
        "direction=up",
    )
    .await
    .status()
}

async fn body_text(response: Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

#[tokio::test]
async fn anonymous_timeline_vote_is_unauthorized() {
    let response = send(
        Method::POST,
        "/api/timeline/t1/vote",
        "application/x-www-form-urlencoded",
        "direction=up",
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8",
        "a refusal is never an HTML fragment"
    );
    assert_eq!(
        body_text(response).await,
        "sign in to continue",
        "app.js turns this 401 into the sign-in prompt"
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
async fn identity_is_checked_before_the_vote_body() {
    // A signed-out reader gets the same 401 whatever the body looks like, so the
    // browser always has one refusal shape to explain.
    let response = send(
        Method::POST,
        "/api/timeline/t1/vote",
        "text/plain",
        "not a vote at all",
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_text(response).await, "sign in to continue");
}

#[tokio::test]
async fn anonymous_own_votes_is_unauthorized() {
    let response = send(Method::GET, "/api/timeline/t1/votes", "", "").await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        body_text(response).await,
        "sign in to continue",
        "app.js ignores this 401 and leaves the widgets neutral"
    );
}

#[tokio::test]
async fn timeline_vote_is_post_only() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/vote").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn event_vote_is_post_only() {
    assert_eq!(
        request(Method::GET, "/api/timeline/t1/event/e1/vote").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn own_votes_read_is_get_only() {
    assert_eq!(
        request(Method::POST, "/api/timeline/t1/votes").await,
        StatusCode::METHOD_NOT_ALLOWED
    );
}
