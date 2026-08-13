//! Authenticated encryption for sensitive pending profile-change payloads.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use kabipay_common::{KabiPayError, KabiPayResult};
use rand::{rngs::OsRng, RngCore};
use serde_json::Value;
use uuid::Uuid;

const NONCE_LEN: usize = 12;
pub const PAYLOAD_ENCRYPTION_VERSION: i16 = 1;
const KEY_ENV: &str = "KABIPAY_PROFILE_CHANGE_ENCRYPTION_KEY";

pub struct PayloadContext<'a> {
    pub tenant_id: Uuid,
    pub request_id: Uuid,
    pub request_type: &'a str,
}

impl PayloadContext<'_> {
    fn associated_data(&self) -> Vec<u8> {
        format!(
            "employee-profile-change:v{}:{}:{}:{}",
            PAYLOAD_ENCRYPTION_VERSION, self.tenant_id, self.request_id, self.request_type
        )
        .into_bytes()
    }
}

pub struct ProfilePayloadCipher {
    cipher: Aes256Gcm,
}

impl ProfilePayloadCipher {
    pub fn from_env() -> KabiPayResult<Self> {
        let encoded = std::env::var(KEY_ENV).map_err(|_| {
            KabiPayError::Internal(format!(
                "{KEY_ENV} must be configured before accepting profile changes"
            ))
        })?;
        let decoded = STANDARD.decode(encoded.trim()).map_err(|_| {
            KabiPayError::Internal(format!("{KEY_ENV} must be a base64-encoded 32-byte key"))
        })?;
        let key: [u8; 32] = decoded.try_into().map_err(|_| {
            KabiPayError::Internal(format!("{KEY_ENV} must decode to exactly 32 bytes"))
        })?;
        Ok(Self::from_key_bytes(key))
    }

    fn from_key_bytes(key: [u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new((&key).into()),
        }
    }

    pub fn encrypt(&self, context: &PayloadContext<'_>, value: &Value) -> KabiPayResult<Vec<u8>> {
        let plaintext = serde_json::to_vec(value)?;
        let mut nonce_bytes = [0_u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let encrypted = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &plaintext,
                    aad: &context.associated_data(),
                },
            )
            .map_err(|_| KabiPayError::Internal("profile payload encryption failed".into()))?;
        let mut envelope = Vec::with_capacity(NONCE_LEN + encrypted.len());
        envelope.extend_from_slice(&nonce_bytes);
        envelope.extend_from_slice(&encrypted);
        Ok(envelope)
    }

    pub fn decrypt(&self, context: &PayloadContext<'_>, envelope: &[u8]) -> KabiPayResult<Value> {
        if envelope.len() <= NONCE_LEN {
            return Err(KabiPayError::Internal(
                "encrypted profile payload is invalid".into(),
            ));
        }
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&envelope[..NONCE_LEN]),
                Payload {
                    msg: &envelope[NONCE_LEN..],
                    aad: &context.associated_data(),
                },
            )
            .map_err(|_| {
                KabiPayError::Internal(
                    "encrypted profile payload failed integrity verification".into(),
                )
            })?;
        serde_json::from_slice(&plaintext).map_err(KabiPayError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_payload_round_trips_and_rejects_tampering() {
        let cipher = ProfilePayloadCipher::from_key_bytes([7_u8; 32]);
        let aad = PayloadContext {
            tenant_id: uuid::Uuid::nil(),
            request_id: uuid::Uuid::from_u128(1),
            request_type: "PAN",
        };
        let payload = serde_json::json!({"pan_number": "ABCDE1234F"});
        let mut encrypted = cipher.encrypt(&aad, &payload).expect("encrypt");
        assert_eq!(cipher.decrypt(&aad, &encrypted).expect("decrypt"), payload);
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;
        assert!(cipher.decrypt(&aad, &encrypted).is_err());
    }
}
