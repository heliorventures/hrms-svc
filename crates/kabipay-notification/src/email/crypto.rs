use super::keyring::{Keyring, SecretString};
use aes_gcm::{
    aead::{
        rand_core::{OsRng, RngCore},
        Aead, KeyInit, Payload,
    },
    Aes256Gcm, Nonce,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use uuid::Uuid;

pub(crate) struct EncryptedCredential {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub cipher: Vec<u8>,
}
impl std::fmt::Debug for EncryptedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EncryptedCredential([redacted])")
    }
}
fn invalid() -> KabiPayError {
    KabiPayError::BusinessRule {
        code: "EMAIL_CREDENTIAL_UNAVAILABLE",
        message:
            "The saved email credential could not be opened. Contact the system administrator."
                .into(),
    }
}
fn aad(tenant: Uuid, settings: Uuid, key_id: &str, purpose: &str) -> KabiPayResult<Vec<u8>> {
    serde_json::to_vec(&(purpose, tenant, settings, key_id)).map_err(|_| invalid())
}
pub(crate) fn seal_email_credential(
    tenant: Uuid,
    settings: Uuid,
    key_id: &str,
    secret: &SecretString,
    keys: &Keyring,
) -> KabiPayResult<EncryptedCredential> {
    let cipher = Aes256Gcm::new_from_slice(keys.key(tenant, key_id)?).map_err(|_| invalid())?;
    let mut nonce = [0_u8; 12];
    OsRng.try_fill_bytes(&mut nonce).map_err(|_| invalid())?;
    let aad = aad(tenant, settings, key_id, "tenant-email-credential-v1")?;
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: secret.expose().as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| invalid())?;
    Ok(EncryptedCredential {
        key_id: key_id.into(),
        nonce: nonce.to_vec(),
        cipher: encrypted,
    })
}
pub(crate) fn open_email_credential(
    tenant: Uuid,
    settings: Uuid,
    encrypted: &EncryptedCredential,
    keys: &Keyring,
) -> KabiPayResult<SecretString> {
    open_with_purpose(
        tenant,
        settings,
        encrypted,
        keys,
        "tenant-email-credential-v1",
    )
}
fn open_with_purpose(
    tenant: Uuid,
    settings: Uuid,
    encrypted: &EncryptedCredential,
    keys: &Keyring,
    purpose: &str,
) -> KabiPayResult<SecretString> {
    if encrypted.nonce.len() != 12 || !(17..=1040).contains(&encrypted.cipher.len()) {
        return Err(invalid());
    }
    let cipher =
        Aes256Gcm::new_from_slice(keys.key(tenant, &encrypted.key_id)?).map_err(|_| invalid())?;
    let aad = aad(tenant, settings, &encrypted.key_id, purpose)?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            Payload {
                msg: &encrypted.cipher,
                aad: &aad,
            },
        )
        .map_err(|_| invalid())?;
    SecretString::new(String::from_utf8(plain).map_err(|_| invalid())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::keyring::Keyring;
    use uuid::Uuid;

    #[test]
    fn wrong_tenant_setting_key_or_purpose_fails_authenticated_decryption() {
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        let settings = Uuid::new_v4();
        let mut document = serde_json::json!({"version":1,"tenants":{}});
        for id in [tenant, other] {
            document["tenants"][id.to_string()] = serde_json::json!({"active_key_id":"v1","keys":{
                "v1":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "v2":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}});
        }
        let keys = Keyring::from_json(&serde_json::to_vec(&document).unwrap()).unwrap();
        let secret = SecretString::new("synthetic-credential".into()).unwrap();
        let mut sealed = seal_email_credential(tenant, settings, "v1", &secret, &keys).unwrap();
        assert_eq!(
            open_email_credential(tenant, settings, &sealed, &keys)
                .unwrap()
                .expose(),
            secret.expose()
        );
        assert!(open_email_credential(tenant, Uuid::new_v4(), &sealed, &keys).is_err());
        assert!(open_email_credential(other, settings, &sealed, &keys).is_err());
        assert!(open_with_purpose(tenant, settings, &sealed, &keys, "different-purpose").is_err());
        let again = seal_email_credential(tenant, settings, "v1", &secret, &keys).unwrap();
        assert_ne!(sealed.nonce, again.nonce);
        assert!(!format!("{sealed:?}").contains("synthetic-credential"));
        sealed.key_id = "v2".into();
        assert!(open_email_credential(tenant, settings, &sealed, &keys).is_err());
        sealed.key_id = "v1".into();
        sealed.nonce[0] ^= 1;
        assert!(open_email_credential(tenant, settings, &sealed, &keys).is_err());
        sealed.nonce[0] ^= 1;
        sealed.cipher[0] ^= 1;
        assert!(open_email_credential(tenant, settings, &sealed, &keys).is_err());
    }
}
