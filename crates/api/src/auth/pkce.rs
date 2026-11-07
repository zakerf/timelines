//! PKCE (RFC 7636) and opaque-token generation.
//!
//! The verifier never leaves the server; only its SHA-256 challenge travels in
//! the authorization redirect. The same generator produces the `state` value
//! and the opaque session id.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Bytes of entropy behind a verifier. 32 bytes encode to the 43-character
/// minimum length RFC 7636 requires.
const VERIFIER_BYTES: usize = 32;
/// Bytes behind a `state` value / session id.
const OPAQUE_TOKEN_BYTES: usize = 32;

/// A fresh PKCE `code_verifier` (43 base64url characters).
pub fn generate_verifier() -> String {
    random_token(VERIFIER_BYTES)
}

/// A fresh opaque value for `state`, session ids, and similar handshakes.
pub fn generate_token() -> String {
    random_token(OPAQUE_TOKEN_BYTES)
}

/// The S256 challenge for a verifier: base64url(SHA-256(verifier)).
pub fn generate_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_7636_s256_test_vector() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

        assert_eq!(generate_challenge(verifier), challenge);
    }

    #[test]
    fn generated_verifiers_are_rfc_compliant_and_unique() {
        let first = generate_verifier();
        let second = generate_verifier();

        assert_eq!(first.len(), 43);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~')),
            "verifier uses the unreserved charset: {first}"
        );
        assert_ne!(first, second);
        assert_eq!(generate_challenge(&first).len(), 43);
    }

    #[test]
    fn opaque_tokens_are_url_safe_and_unique() {
        let first = generate_token();
        let second = generate_token();

        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
        assert!(!first.contains('='));
    }
}
