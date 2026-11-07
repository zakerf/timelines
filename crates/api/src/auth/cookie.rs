//! The one browser-visible piece of auth: an opaque session cookie.
//!
//! No token ever reaches JavaScript. `HttpOnly` keeps it away from scripts,
//! `Secure` keeps it off plaintext transports (browsers still accept it on
//! localhost), and `SameSite=Lax` pairs with the same-origin `/api/*` mount.

use axum::http::HeaderMap;
use axum::http::header::COOKIE;

/// Short-lived cookie binding an in-flight authorization request to the
/// browser that started it. Without it, a server-side-only `state` still
/// validates the callback but not the browser (login CSRF).
pub const OAUTH_STATE_COOKIE_NAME: &str = "timeline_oauth_state";

/// Read one cookie value out of a `Cookie` header.
pub fn value_from_headers(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(COOKIE)?.to_str().ok()?;
    value_from_header(header, name)
}

/// Read one cookie value out of a raw `Cookie` header value.
pub fn value_from_header(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}

/// `Set-Cookie` for a freshly created session.
pub fn session_cookie(name: &str, session_id: &str, max_age_seconds: i64) -> String {
    format!(
        "{name}={session_id}; Path=/; Max-Age={max_age_seconds}; HttpOnly; Secure; SameSite=Lax"
    )
}

/// `Set-Cookie` that removes the session cookie from the browser.
pub fn clear_cookie(name: &str) -> String {
    format!("{name}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
}

/// `Set-Cookie` for the browser-bound `state` value. Scoped to the auth
/// endpoints and short-lived; it never contains a token.
pub fn state_cookie(state: &str, max_age_seconds: i64) -> String {
    format!(
        "{OAUTH_STATE_COOKIE_NAME}={state}; Path=/api/auth; Max-Age={max_age_seconds}; \
         HttpOnly; Secure; SameSite=Lax"
    )
}

/// Remove the in-flight `state` cookie after the callback.
pub fn clear_state_cookie() -> String {
    format!("{OAUTH_STATE_COOKIE_NAME}=; Path=/api/auth; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_carries_the_required_attributes() {
        let cookie = session_cookie("timeline_session", "abc123", 86_400);

        assert_eq!(
            cookie,
            "timeline_session=abc123; Path=/; Max-Age=86400; HttpOnly; Secure; SameSite=Lax"
        );
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
    }

    #[test]
    fn clearing_expires_the_cookie_immediately() {
        let cookie = clear_cookie("timeline_session");

        assert!(cookie.starts_with("timeline_session=;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn state_cookie_is_scoped_to_the_auth_routes() {
        let cookie = state_cookie("state-1", 600);

        assert!(cookie.starts_with("timeline_oauth_state=state-1;"));
        assert!(cookie.contains("Path=/api/auth"));
        assert!(cookie.contains("Max-Age=600"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(clear_state_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn reads_one_cookie_and_ignores_neighbours() {
        let header = "theme=dark; timeline_session=sess-1; other=x";

        assert_eq!(
            value_from_header(header, "timeline_session"),
            Some("sess-1".to_owned())
        );
        assert_eq!(value_from_header(header, "missing"), None);
    }

    #[test]
    fn handles_a_missing_cookie_header() {
        let headers = HeaderMap::new();
        assert_eq!(value_from_headers(&headers, "timeline_session"), None);
    }
}
