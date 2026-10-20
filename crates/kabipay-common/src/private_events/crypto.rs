//! AES256-GCM source-byte contract. Key material and plaintext have no Debug output.
use crate::{KabiPayError, KabiPayResult};
use aes_gcm::{
    aead::{
        rand_core::{OsRng, RngCore},
        Aead, KeyInit, Payload,
    },
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const FRAME_BYTES: usize = 256 * 1024;
fn invalid() -> KabiPayError {
    KabiPayError::Internal("invalid private event".into())
}
pub fn source_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn purpose(s: &str) -> bool {
    matches!(s, "audit" | "delivery" | "operator_audit")
}
fn decode(s: &str, max: usize) -> KabiPayResult<Vec<u8>> {
    if s.len() > max.div_ceil(3) * 4 {
        return Err(invalid());
    }
    let b = STANDARD.decode(s).map_err(|_| invalid())?;
    if b.len() > max || STANDARD.encode(&b) != s {
        return Err(invalid());
    }
    Ok(b)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHead {
    pub record_id: Uuid,
    pub tenant_id: Uuid,
    pub purpose: String,
    pub source_generation: i64,
    pub source_kind: String,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub occurred_at: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema_version: u8,
    pub event_id: Uuid,
    pub record_id: Uuid,
    pub tenant_id: Uuid,
    pub purpose: String,
    pub source_generation: i64,
    pub source_kind: String,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub occurred_at: String,
    pub key_id: String,
    pub source_sha256: String,
    pub source_size: usize,
    pub frame_index: usize,
    pub frame_count: usize,
    pub nonce: String,
    pub ciphertext: String,
}
impl Envelope {
    fn validate(&self) -> KabiPayResult<()> {
        let time =
            chrono::DateTime::parse_from_rfc3339(&self.occurred_at).map_err(|_| invalid())?;
        if self.schema_version != 1
            || [
                self.event_id,
                self.record_id,
                self.tenant_id,
                self.resource_id,
            ]
            .iter()
            .any(Uuid::is_nil)
            || !purpose(&self.purpose)
            || self.source_generation < 1
            || self.source_generation > 9_007_199_254_740_991
            || ![&self.source_kind, &self.resource_type, &self.key_id]
                .iter()
                .all(|s| identifier(s))
            || time
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
                != self.occurred_at
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.source_size == 0
            || self.source_size > MAX_SOURCE_BYTES
            || self.frame_count != self.source_size.div_ceil(FRAME_BYTES)
            || self.frame_index >= self.frame_count
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn associated_data(&self) -> KabiPayResult<Vec<u8>> {
        self.validate()?;
        let fields = [
            self.schema_version.to_string(),
            self.event_id.to_string(),
            self.record_id.to_string(),
            self.tenant_id.to_string(),
            self.purpose.clone(),
            self.source_generation.to_string(),
            self.source_kind.clone(),
            self.resource_type.clone(),
            self.resource_id.to_string(),
            self.occurred_at.clone(),
            self.key_id.clone(),
            self.source_sha256.clone(),
            self.source_size.to_string(),
            self.frame_index.to_string(),
            self.frame_count.to_string(),
        ];
        let mut data = b"kabipay-private-event-v1\0".to_vec();
        for f in fields {
            data.extend_from_slice(format!("{}:", f.len()).as_bytes());
            data.extend_from_slice(f.as_bytes());
        }
        Ok(data)
    }
    fn same_record(&self, b: &Self) -> bool {
        self.record_id == b.record_id
            && self.tenant_id == b.tenant_id
            && self.purpose == b.purpose
            && self.source_generation == b.source_generation
            && self.source_kind == b.source_kind
            && self.resource_type == b.resource_type
            && self.resource_id == b.resource_id
            && self.occurred_at == b.occurred_at
            && self.key_id == b.key_id
            && self.source_sha256 == b.source_sha256
            && self.source_size == b.source_size
            && self.frame_count == b.frame_count
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    schema_version: u8,
    keys: Vec<KeyEntry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    tenant_id: Uuid,
    purpose: String,
    key_id: String,
    key_b64: String,
    active: bool,
}
pub struct Keyring {
    keys: BTreeMap<(Uuid, String, String), [u8; 32]>,
    active: BTreeMap<(Uuid, String), String>,
}
impl Keyring {
    pub fn from_json(bytes: &[u8]) -> KabiPayResult<Self> {
        if bytes.len() > 65536 {
            return Err(invalid());
        }
        let file: KeyFile = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if file.schema_version != 1 || file.keys.is_empty() || file.keys.len() > 256 {
            return Err(invalid());
        }
        let mut keys = BTreeMap::new();
        let mut active = BTreeMap::new();
        let mut scopes = BTreeSet::new();
        for k in file.keys {
            if k.tenant_id.is_nil() || !purpose(&k.purpose) || !identifier(&k.key_id) {
                return Err(invalid());
            }
            let b = decode(&k.key_b64, 32)?;
            let key: [u8; 32] = b.try_into().map_err(|_| invalid())?;
            let scope = (k.tenant_id, k.purpose.clone());
            scopes.insert(scope.clone());
            if keys
                .insert((k.tenant_id, k.purpose, k.key_id.clone()), key)
                .is_some()
            {
                return Err(invalid());
            }
            if k.active && active.insert(scope, k.key_id).is_some() {
                return Err(invalid());
            }
        }
        if scopes.len() != active.len() {
            return Err(invalid());
        }
        Ok(Self { keys, active })
    }
    pub fn encrypt_frames(&self, source: &[u8], head: &SourceHead) -> KabiPayResult<Vec<Envelope>> {
        if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
            return Err(invalid());
        }
        let key_id = self
            .active
            .get(&(head.tenant_id, head.purpose.clone()))
            .ok_or_else(invalid)?;
        let key = self
            .keys
            .get(&(head.tenant_id, head.purpose.clone(), key_id.clone()))
            .ok_or_else(invalid)?;
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| invalid())?;
        let hash = source_hash(source);
        let mut frames = Vec::with_capacity(source.len().div_ceil(FRAME_BYTES));
        for (index, bytes) in source.chunks(FRAME_BYTES).enumerate() {
            let mut h = Envelope {
                schema_version: 1,
                event_id: Uuid::new_v4(),
                record_id: head.record_id,
                tenant_id: head.tenant_id,
                purpose: head.purpose.clone(),
                source_generation: head.source_generation,
                source_kind: head.source_kind.clone(),
                resource_type: head.resource_type.clone(),
                resource_id: head.resource_id,
                occurred_at: head.occurred_at.clone(),
                key_id: key_id.clone(),
                source_sha256: hash.clone(),
                source_size: source.len(),
                frame_index: index,
                frame_count: source.len().div_ceil(FRAME_BYTES),
                nonce: String::new(),
                ciphertext: String::new(),
            };
            let mut nonce = [0; 12];
            OsRng.try_fill_bytes(&mut nonce).map_err(|_| invalid())?;
            let c = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: bytes,
                        aad: &h.associated_data()?,
                    },
                )
                .map_err(|_| invalid())?;
            h.nonce = STANDARD.encode(nonce);
            h.ciphertext = STANDARD.encode(c);
            frames.push(h);
        }
        Ok(frames)
    }
    pub fn decrypt_frames(
        &self,
        frames: &[Envelope],
        tenant: Uuid,
        p: &str,
    ) -> KabiPayResult<Vec<u8>> {
        let first = frames.first().ok_or_else(invalid)?;
        if frames.len() > 64 {
            return Err(invalid());
        }
        let mut parts = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for h in frames {
            h.validate()?;
            if h.tenant_id != tenant
                || h.purpose != p
                || !first.same_record(h)
                || !ids.insert(h.event_id)
                || parts.contains_key(&h.frame_index)
            {
                return Err(invalid());
            }
            let key = self
                .keys
                .get(&(tenant, p.into(), h.key_id.clone()))
                .ok_or_else(invalid)?;
            let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| invalid())?;
            let n = decode(&h.nonce, 12)?;
            if n.len() != 12 {
                return Err(invalid());
            }
            let c = decode(&h.ciphertext, FRAME_BYTES + 16)?;
            let bytes = cipher
                .decrypt(
                    Nonce::from_slice(&n),
                    Payload {
                        msg: &c,
                        aad: &h.associated_data()?,
                    },
                )
                .map_err(|_| invalid())?;
            if bytes.len() != FRAME_BYTES.min(h.source_size - h.frame_index * FRAME_BYTES) {
                return Err(invalid());
            }
            parts.insert(h.frame_index, bytes);
        }
        if parts.len() != first.frame_count {
            return Err(invalid());
        }
        let source: Vec<u8> = parts.into_values().flatten().collect();
        if source.len() != first.source_size || source_hash(&source) != first.source_sha256 {
            return Err(invalid());
        }
        Ok(source)
    }
}
