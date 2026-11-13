//! Shared HTTP plumbing used across every route.
//!
//! One module owns the cross-cutting pieces the write path repeats, so the
//! modules never drift apart:
//!
//! - the trusted-edge client IP (`CloudFront-Viewer-Address`) and its
//!   `sha256(ip + salt)` hash (PRD §8) — the security-sensitive boundary that
//!   must exist in exactly one place;
//! - HTML escaping for HTMX fragments;
//! - the no-store response writers every fragment and refusal uses (PRD §4).

use std::net::IpAddr;

use axum::body::Body;
use axum::http::HeaderMap;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::Response;
use sha2::{Digest, Sha256};

/// CloudFront's trusted client address header. API Gateway deployments pass
/// their `sourceIp` through the same slot in the Lambda adapter.
pub const CLOUDFRONT_VIEWER_ADDRESS: &str = "cloudfront-viewer-address";

/// `sha256(clientIp + salt)`, lowercase hex. That exact input format is the
/// spec's privacy boundary; the raw IP never enters DynamoDB.
pub fn hash_ip(ip: &str, salt: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(ip.as_bytes());
    digest.update(salt.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Read the client IP from the trusted edge only.
///
/// `CloudFront-Viewer-Address` is overwritten by CloudFront, so it cannot be
/// spoofed by the browser. `X-Forwarded-For` is intentionally not consulted.
pub fn trusted_client_ip(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(CLOUDFRONT_VIEWER_ADDRESS)?.to_str().ok()?;
    parse_edge_address(raw)
}

fn parse_edge_address(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // A bare IP (no port) is the simplest form.
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Some(ip.to_string());
    }

    // IPv6 arrives bracketed: `[2001:db8::1]:443`.
    if let Some(rest) = raw.strip_prefix('[') {
        let (address, _) = rest.split_once(']')?;
        return address.parse::<IpAddr>().ok().map(|ip| ip.to_string());
    }

    // IPv4 arrives as `203.0.113.5:12345`.
    let (address, _) = raw.rsplit_once(':')?;
    address.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// Escape user text before it is embedded in an HTML fragment. Prevents a
/// stored value from injecting markup into the HTMX response.
pub fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A response with the given status and content type, always `no-store` so a
/// per-user fragment can never enter a shared cache (PRD §4).
pub fn response_with(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Body>,
) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("static response is valid")
}

/// `text/html` fragment.
pub fn html_response(status: StatusCode, body: impl Into<Body>) -> Response {
    response_with(status, "text/html; charset=utf-8", body)
}

/// `application/json` body.
pub fn json_response(status: StatusCode, value: &serde_json::Value) -> Response {
    response_with(
        status,
        "application/json; charset=utf-8",
        value.to_string(),
    )
}

/// Coarse `text/plain` refusal body.
pub fn plain(status: StatusCode, body: impl Into<String>) -> Response {
    response_with(status, "text/plain; charset=utf-8", body.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_hashing_is_sha256_of_ip_plus_salt() {
        // Independent vector: sha256("127.0.0.1dev-salt").
        assert_eq!(
            hash_ip("127.0.0.1", "dev-salt"),
            "1f1643dde2f527bff1ee67c68eb0ba45ab2a9ab2fe787f519a22a7d296b4fbed"
        );
        let hash = hash_ip("203.0.113.9", "salt-a");
        assert_ne!(hash, hash_ip("203.0.113.9", "salt-b"), "the salt matters");
        assert_eq!(hash, hash_ip("203.0.113.9", "salt-a"), "and it is stable");
        assert!(
            !hash.contains("203.0.113.9"),
            "the raw address never survives the hash"
        );
    }

    #[test]
    fn cloudfront_viewer_address_is_parsed_with_or_without_a_port() {
        assert_eq!(
            parse_edge_address("203.0.113.5:12345").as_deref(),
            Some("203.0.113.5")
        );
        assert_eq!(
            parse_edge_address("203.0.113.5").as_deref(),
            Some("203.0.113.5")
        );
        assert_eq!(
            parse_edge_address("[2001:db8::1]:443").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(parse_edge_address("not-an-ip:1"), None);
    }

    #[test]
    fn trusted_ip_comes_from_the_edge_header_only() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.9".parse().unwrap());
        assert_eq!(trusted_client_ip(&headers), None);

        headers.insert(
            CLOUDFRONT_VIEWER_ADDRESS,
            "203.0.113.5:443".parse().unwrap(),
        );
        assert_eq!(
            trusted_client_ip(&headers).as_deref(),
            Some("203.0.113.5"),
            "CloudFront wins over the client header"
        );
    }

    #[test]
    fn escaping_neutralizes_markup() {
        let escaped = escape_html("<script>alert('x')</script>");
        assert!(!escaped.contains("<script>"));
        assert!(escaped.contains("&lt;script&gt;"));
    }

    #[test]
    fn responses_are_no_store() {
        assert_eq!(
            html_response(StatusCode::OK, "").headers()[CACHE_CONTROL],
            "no-store"
        );
        assert_eq!(
            json_response(StatusCode::OK, &serde_json::json!({})).headers()[CACHE_CONTROL],
            "no-store"
        );
        assert_eq!(
            plain(StatusCode::OK, "").headers()[CACHE_CONTROL],
            "no-store"
        );
    }
}
