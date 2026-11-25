//! Dedicated Notification-only key material. Never serialized or logged.
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::{KabiPayError, KabiPayResult};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer,
};
use std::path::Path;
use std::{collections::BTreeMap, fmt, marker::PhantomData};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

pub struct SecretString(String);
impl SecretString {
    pub fn new(value: String) -> KabiPayResult<Self> {
        if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(KabiPayError::Validation("invalid email credential".into()));
        }
        Ok(Self(value))
    }
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    #[serde(deserialize_with = "unique_map")]
    tenants: BTreeMap<Uuid, TenantConfig>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TenantConfig {
    active_key_id: String,
    #[serde(deserialize_with = "unique_map")]
    keys: BTreeMap<String, String>,
}
struct TenantKeys {
    active: String,
    keys: BTreeMap<String, [u8; 32]>,
}
pub struct Keyring {
    tenants: BTreeMap<Uuid, TenantKeys>,
}
impl fmt::Debug for Keyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Keyring([redacted])")
    }
}

/// Reject duplicate JSON keys rather than silently selecting one credential.
fn unique_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct MapVisitor<K, V>(PhantomData<(K, V)>);
    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Visitor<'de> for MapVisitor<K, V> {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a unique-key object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate key"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(MapVisitor(PhantomData))
}

pub(crate) fn unavailable() -> KabiPayError {
    KabiPayError::BusinessRule {
        code: "EMAIL_KEY_UNAVAILABLE",
        message: "Email encryption is not configured. Contact the system administrator.".into(),
    }
}
pub(crate) fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

impl Keyring {
    pub async fn from_file(path: &Path) -> KabiPayResult<Self> {
        Self::from_json(&read_private_file(path, 1024 * 1024).await?)
    }
    pub fn from_json(bytes: &[u8]) -> KabiPayResult<Self> {
        if bytes.len() > 1024 * 1024 {
            return Err(unavailable());
        }
        let config: Config = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        if config.version != 1 || config.tenants.is_empty() || config.tenants.len() > 1000 {
            return Err(unavailable());
        }
        let mut tenants = BTreeMap::new();
        for (tenant, config) in config.tenants {
            if tenant.is_nil()
                || config.keys.is_empty()
                || config.keys.len() > 16
                || !config.keys.contains_key(&config.active_key_id)
            {
                return Err(unavailable());
            }
            let mut keys = BTreeMap::new();
            for (id, encoded) in config.keys {
                if !valid_key_id(&id) {
                    return Err(unavailable());
                }
                let bytes = STANDARD.decode(&encoded).map_err(|_| unavailable())?;
                if bytes.len() != 32 || STANDARD.encode(&bytes) != encoded {
                    return Err(unavailable());
                }
                let key: [u8; 32] = bytes.try_into().map_err(|_| unavailable())?;
                keys.insert(id, key);
            }
            tenants.insert(
                tenant,
                TenantKeys {
                    active: config.active_key_id,
                    keys,
                },
            );
        }
        Ok(Self { tenants })
    }
    pub(crate) fn active_id(&self, tenant: Uuid) -> KabiPayResult<&str> {
        self.tenants
            .get(&tenant)
            .map(|keys| keys.active.as_str())
            .ok_or_else(unavailable)
    }
    pub(crate) fn key(&self, tenant: Uuid, id: &str) -> KabiPayResult<&[u8; 32]> {
        self.tenants
            .get(&tenant)
            .and_then(|keys| keys.keys.get(id))
            .ok_or_else(unavailable)
    }
}

pub(crate) async fn read_private_file(path: &Path, limit: u64) -> KabiPayResult<Vec<u8>> {
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unavailable());
        }
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| unavailable())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > limit {
        return Err(unavailable());
    }
    Ok(bytes)
}
pub(crate) async fn configured_keyring() -> KabiPayResult<Keyring> {
    let path = std::env::var_os("KABIPAY_EMAIL_KEYRING_FILE").ok_or_else(unavailable)?;
    Keyring::from_file(Path::new(&path)).await
}
