//! Refresh-token encryption at rest (PRD §7).
//!
//! Production uses KMS (`AWS_ENDPOINT_URL` points at floci locally). When no
//! key is configured the seam falls back to an explicit local-only marker so a
//! developer can run the whole flow without provisioning KMS; the ciphertext
//! shape still round-trips through the same account attribute.

use aws_sdk_kms::Client as KmsClient;
use aws_sdk_kms::primitives::Blob;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::error::AuthError;

/// Either a real KMS key or the local-only fallback.
#[derive(Clone)]
pub enum RefreshTokenCipher {
    Kms { client: KmsClient, key_id: String },
    Local,
}

impl RefreshTokenCipher {
    /// Encrypt a refresh token into the value stored on the account row.
    pub async fn encrypt(&self, plaintext: &str) -> Result<String, AuthError> {
        match self {
            Self::Local => Ok(format!("local:{}", STANDARD.encode(plaintext.as_bytes()))),
            Self::Kms { client, key_id } => {
                let output = client
                    .encrypt()
                    .key_id(key_id)
                    .plaintext(Blob::new(plaintext.as_bytes()))
                    .send()
                    .await
                    .map_err(|error| {
                        AuthError::internal(format!("KMS encrypt failed: {error:?}"))
                    })?;
                let ciphertext = output
                    .ciphertext_blob()
                    .ok_or_else(|| AuthError::internal("KMS encrypt returned no ciphertext"))?;
                Ok(format!("kms:{}", STANDARD.encode(ciphertext.as_ref())))
            }
        }
    }

    /// Decrypt a stored ciphertext (used only for revocation on logout).
    pub async fn decrypt(&self, stored: &str) -> Result<String, AuthError> {
        if let Some(encoded) = stored.strip_prefix("local:") {
            let bytes = STANDARD.decode(encoded).map_err(|error| {
                AuthError::internal(format!("invalid local ciphertext: {error}"))
            })?;
            return String::from_utf8(bytes)
                .map_err(|error| AuthError::internal(format!("invalid local plaintext: {error}")));
        }

        let encoded = stored
            .strip_prefix("kms:")
            .ok_or_else(|| AuthError::internal("refresh token has an unknown ciphertext prefix"))?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|error| AuthError::internal(format!("invalid KMS ciphertext: {error}")))?;

        match self {
            Self::Local => Err(AuthError::internal(
                "KMS ciphertext found but no KMS key is configured",
            )),
            Self::Kms { client, key_id } => {
                let output = client
                    .decrypt()
                    .key_id(key_id)
                    .ciphertext_blob(Blob::new(bytes))
                    .send()
                    .await
                    .map_err(|error| {
                        AuthError::internal(format!("KMS decrypt failed: {error:?}"))
                    })?;
                let plaintext = output
                    .plaintext()
                    .ok_or_else(|| AuthError::internal("KMS decrypt returned no plaintext"))?;
                String::from_utf8(plaintext.as_ref().to_vec())
                    .map_err(|error| AuthError::internal(format!("invalid plaintext: {error}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_fallback_round_trips_and_is_marked() {
        let cipher = RefreshTokenCipher::Local;

        let encrypted = cipher
            .encrypt("refresh-token-value")
            .await
            .expect("encrypt");
        assert!(encrypted.starts_with("local:"));
        assert!(!encrypted.contains("refresh-token-value"));

        let decrypted = cipher.decrypt(&encrypted).await.expect("decrypt");
        assert_eq!(decrypted, "refresh-token-value");
    }

    #[tokio::test]
    async fn unknown_ciphertext_prefix_is_rejected() {
        let error = RefreshTokenCipher::Local
            .decrypt("plain-token")
            .await
            .expect_err("unmarked ciphertext must fail");

        assert!(error.to_string().contains("prefix"), "{error}");
    }
}
