//! Source-only signed HTTPS history transport and transactional pending overlay.
//! No shared worker receives source PostgreSQL credentials or source key material.
use super::{
    configured_company_keyring,
    crypto::{source_hash, Envelope},
    AuditHistoryFilter, AuditHistoryPage, AuditRecordInput,
};
use crate::{KabiPayError, KabiPayResult};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{collections::BTreeMap, io::Read, time::Duration};
use uuid::Uuid;
const REQUEST_DOMAIN: &str = "kabipay-private-history-request-v1";
const RESPONSE_DOMAIN: &str = "kabipay-private-history-response-v1";
const HISTORY_PATH: &str = "/internal/private-events/history";
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
fn unavailable() -> KabiPayError {
    KabiPayError::Internal("private audit history unavailable or incomplete".into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u8,
    bindings: Vec<Binding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    tenant_id: Uuid,
    url: String,
    key_b64: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    payload: String,
    signature: String,
}
#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    tenant_id: Uuid,
    source_generation: i64,
    request_id: Uuid,
    issued_at: i64,
    filter: &'a AuditHistoryFilter,
    coverage_id: Uuid,
    coverage_sha256: &'a str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    tenant_id: Uuid,
    source_generation: i64,
    request_id: Uuid,
    issued_at: i64,
    coverage_id: Uuid,
    request_hash: String,
    page: RawHistoryPage,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHistoryPage {
    records: Vec<String>,
    total: u64,
}
fn binding(bytes: &[u8], tenant: Uuid) -> KabiPayResult<(reqwest::Url, Vec<u8>)> {
    binding_for_path(bytes, tenant, HISTORY_PATH)
}
pub(super) fn binding_for_path(
    bytes: &[u8],
    tenant: Uuid,
    path: &str,
) -> KabiPayResult<(reqwest::Url, Vec<u8>)> {
    if bytes.len() > 65536 {
        return Err(unavailable());
    }
    let config: Config = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    if config.schema_version != 1 || config.bindings.len() > 256 {
        return Err(unavailable());
    }
    let mut matches = config
        .bindings
        .into_iter()
        .filter(|b| b.tenant_id == tenant);
    let b = matches.next().ok_or_else(unavailable)?;
    if matches.next().is_some() {
        return Err(unavailable());
    }
    let url = reqwest::Url::parse(&b.url).map_err(|_| unavailable())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != path
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(unavailable());
    }
    let key = STANDARD.decode(&b.key_b64).map_err(|_| unavailable())?;
    if key.len() != 32 || STANDARD.encode(&key) != b.key_b64 {
        return Err(unavailable());
    }
    Ok((url, key))
}
fn configured_binding(tenant: Uuid) -> KabiPayResult<(reqwest::Url, Vec<u8>)> {
    let path = crate::db::company_setting(tenant, "KABIPAY_PRIVATE_HISTORY_CLIENT_FILE")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(65537).read_to_end(&mut bytes))
        .map_err(|_| unavailable())?;
    binding(&bytes, tenant)
}
fn configured_pem(name: &str, tenant: Uuid) -> KabiPayResult<Vec<u8>> {
    let path = if name == "KABIPAY_ORGANIZATION_HTTP_CA_FILE" {
        std::env::var(name).map_err(|_| unavailable())?
    } else { crate::db::company_setting(tenant, name)? };
    let path = std::path::Path::new(&path);
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let file = std::fs::File::open(path).map_err(|_| unavailable())?;
    if !file.metadata().map_err(|_| unavailable())?.is_file() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err(unavailable());
    }
    Ok(bytes)
}
pub(super) fn history_http_client(tenant: Uuid) -> KabiPayResult<reqwest::Client> {
    let mut pem = configured_pem("KABIPAY_PRIVATE_HISTORY_CLIENT_CERT_FILE", tenant)?;
    pem.push(b'\n');
    pem.extend(configured_pem("KABIPAY_PRIVATE_HISTORY_CLIENT_KEY_FILE", tenant)?);
    let identity = reqwest::Identity::from_pem(&pem).map_err(|_| unavailable())?;
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .identity(identity)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8));
    if std::env::var_os("KABIPAY_ORGANIZATION_HTTP_CA_FILE").is_some() {
        let bytes = configured_pem("KABIPAY_ORGANIZATION_HTTP_CA_FILE", tenant)?;
        let roots = reqwest::Certificate::from_pem_bundle(&bytes).map_err(|_| unavailable())?;
        if roots.is_empty() || roots.len() > 32 {
            return Err(unavailable());
        }
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
    }
    builder.build().map_err(|_| unavailable())
}
pub(super) fn sign(bytes: &[u8], key: &[u8], domain: &str) -> KabiPayResult<Proof> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| unavailable())?;
    mac.update(domain.as_bytes());
    mac.update(&[0]);
    mac.update(bytes);
    Ok(Proof {
        payload: URL_SAFE_NO_PAD.encode(bytes),
        signature: URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
    })
}
pub(super) fn verify(proof: &Proof, key: &[u8], domain: &str) -> KabiPayResult<Vec<u8>> {
    if proof.payload.len() > MAX_RESPONSE_BYTES * 4 / 3 + 4 || proof.signature.len() != 43 {
        return Err(unavailable());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&proof.payload)
        .map_err(|_| unavailable())?;
    let signature = URL_SAFE_NO_PAD
        .decode(&proof.signature)
        .map_err(|_| unavailable())?;
    if URL_SAFE_NO_PAD.encode(&bytes) != proof.payload
        || URL_SAFE_NO_PAD.encode(&signature) != proof.signature
    {
        return Err(unavailable());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| unavailable())?;
    mac.update(domain.as_bytes());
    mac.update(&[0]);
    mac.update(&bytes);
    mac.verify_slice(&signature).map_err(|_| unavailable())?;
    Ok(bytes)
}
fn matches_filter(row: &AuditRecordInput, f: &AuditHistoryFilter) -> bool {
    row.tenant_id == f.tenant_id
        && f.entity_type.as_ref().is_none_or(|v| row.entity_type == *v)
        && f.entity_id.is_none_or(|v| row.entity_id == Some(v))
        && (f.actions.is_empty() || f.actions.contains(&row.action))
        && f.since.is_none_or(|v| row.created_at >= v)
        && f.until.is_none_or(|v| row.created_at <= v)
}
fn insert_record(
    records: &mut BTreeMap<Uuid, AuditRecordInput>,
    row: AuditRecordInput,
) -> KabiPayResult<()> {
    if row.id.is_nil() {
        return Err(unavailable());
    }
    if let Some(prior) = records.get(&row.id) {
        if serde_json::to_value(prior)? != serde_json::to_value(&row)? {
            return Err(KabiPayError::Conflict(
                "private history record identity has conflicting content".into(),
            ));
        }
    } else {
        records.insert(row.id, row);
    }
    Ok(())
}
fn finish(
    records: BTreeMap<Uuid, AuditRecordInput>,
    f: &AuditHistoryFilter,
) -> Vec<AuditRecordInput> {
    let mut rows: Vec<_> = records
        .into_values()
        .filter(|r| matches_filter(r, f))
        .collect();
    rows.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    rows.into_iter().take(f.limit as usize).collect()
}
/// A consistent local snapshot prevents cleanup racing frame reads. A total deadline
/// and byte budget surface lag explicitly instead of silently truncating history.
pub async fn read_configured_audit_history<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    filter: &AuditHistoryFilter,
) -> KabiPayResult<Vec<AuditRecordInput>> {
    if filter.tenant_id.is_nil()
        || filter.limit == 0
        || filter.limit > 500
        || filter.offset != 0
        || filter.actions.len() > 64
        || filter.since.zip(filter.until).is_some_and(|(a, b)| a > b)
    {
        return Err(KabiPayError::Validation(
            "invalid private history filter".into(),
        ));
    }
    tokio::time::timeout(Duration::from_secs(20), read_inner(db, filter))
        .await
        .map_err(|_| unavailable())?
}
async fn read_inner<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    filter: &AuditHistoryFilter,
) -> KabiPayResult<Vec<AuditRecordInput>> {
    let (url, key) = configured_binding(filter.tenant_id)?;
    let tx = db.begin().await?;
    tx.execute_unprepared("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let row=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT a.producer_generation,e.id AS coverage_id,e.source_sha256 FROM private_source_cutover a JOIN private_legacy_export e ON e.tenant_id=a.tenant_id AND e.producer_generation<=a.producer_generation AND e.source_table='audit_log' AND e.source_version=a.source_version WHERE a.tenant_id=$1 AND e.source_database=current_database() AND e.source_schema=current_schema() AND e.accepted_at<=clock_timestamp() AND e.reader_switch_complete AND e.revoked_at IS NULL AND e.archive_row_count=e.row_count AND e.archive_sha256=e.source_sha256 AND e.restore_sha256=e.source_sha256 AND e.restore_verified_at<=e.accepted_at ORDER BY e.producer_generation DESC,e.accepted_at DESC,e.id DESC LIMIT 1",
        [filter.tenant_id.into()],
    )).await?.ok_or_else(unavailable)?;
    let generation: i64 = row.try_get("", "producer_generation")?;
    let coverage: Uuid = row.try_get("", "coverage_id")?;
    let coverage_hash: String = row.try_get("", "source_sha256")?;
    let keys = configured_company_keyring(filter.tenant_id)?;
    let mut frames: BTreeMap<Uuid, Vec<Envelope>> = BTreeMap::new();
    let mut cursor = Uuid::nil();
    let mut bytes = 0usize;
    loop {
        let rows=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT id,event_envelope FROM company_event_outbox WHERE source_tenant_id=$1 AND event_stream='audit' AND event_envelope->>'source_kind'='audit_log' AND id>$2 ORDER BY id LIMIT 256",
            [filter.tenant_id.into(),cursor.into()],
        )).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            cursor = row.try_get("", "id")?;
            let value: serde_json::Value = row.try_get("", "event_envelope")?;
            bytes = bytes
                .checked_add(serde_json::to_vec(&value)?.len())
                .ok_or_else(unavailable)?;
            if bytes > MAX_RESPONSE_BYTES {
                return Err(KabiPayError::Internal(
                    "private history pending backlog exceeds read budget".into(),
                ));
            }
            let frame: Envelope = serde_json::from_value(value).map_err(|_| unavailable())?;
            if frame.tenant_id != filter.tenant_id
                || frame.purpose != "audit"
                || frame.source_kind != "audit_log"
                || frame.event_id != cursor
                || frame.source_generation > generation
            {
                return Err(unavailable());
            }
            frames.entry(frame.record_id).or_default().push(frame);
        }
    }
    let mut records = BTreeMap::new();
    for (id, frames) in frames {
        let raw = keys.decrypt_frames(&frames, filter.tenant_id, "audit")?;
        let row: AuditRecordInput = serde_json::from_slice(&raw).map_err(|_| unavailable())?;
        if row.id != id
            || row.tenant_id != filter.tenant_id
            || frames.iter().any(|f| {
                f.resource_id != row.entity_id.unwrap_or(row.id)
                    || f.occurred_at
                        != row
                            .created_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
            })
        {
            return Err(unavailable());
        }
        if matches_filter(&row, filter) {
            insert_record(&mut records, row)?;
        }
    }
    tx.commit().await?;
    let client = history_http_client(filter.tenant_id)?;
    let remote_filter = filter.clone();
    {
        let request = Request {
            version: 1,
            tenant_id: filter.tenant_id,
            source_generation: generation,
            request_id: Uuid::new_v4(),
            issued_at: Utc::now().timestamp(),
            filter: &remote_filter,
            coverage_id: coverage,
            coverage_sha256: &coverage_hash,
        };
        let raw = serde_json::to_vec(&request)?;
        let proof = sign(&raw, &key, REQUEST_DOMAIN)?;
        let mut response = client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(&proof)?)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(unavailable());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        let proof: Proof = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        let verified = verify(&proof, &key, RESPONSE_DOMAIN)?;
        let reply: Response = serde_json::from_slice(&verified).map_err(|_| unavailable())?;
        let page = AuditHistoryPage {
            records: reply
                .page
                .records
                .iter()
                .map(|raw| serde_json::from_str(raw).map_err(|_| unavailable()))
                .collect::<KabiPayResult<_>>()?,
            total: reply.page.total,
        };
        if reply.version != 1
            || reply.tenant_id != request.tenant_id
            || reply.source_generation != generation
            || reply.request_id != request.request_id
            || reply.coverage_id != coverage
            || reply.request_hash != source_hash(&raw)
            || Utc::now().timestamp().abs_diff(reply.issued_at) > 60
            || page.records.len() > filter.limit as usize
            || page.total < page.records.len() as u64
        {
            return Err(unavailable());
        }
        if page.records.len() as u64 != page.total.min(filter.limit) {
            return Err(unavailable());
        }
        if page
            .records
            .windows(2)
            .any(|pair| (pair[0].created_at, pair[0].id) <= (pair[1].created_at, pair[1].id))
        {
            return Err(unavailable());
        }
        for row in page.records {
            if !matches_filter(&row, filter) {
                return Err(unavailable());
            }
            bytes = bytes
                .checked_add(serde_json::to_vec(&row)?.len())
                .ok_or_else(unavailable)?;
            if bytes > MAX_RESPONSE_BYTES {
                return Err(unavailable());
            }
            insert_record(&mut records, row)?;
        }
    }
    Ok(finish(records, filter))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_rpc_rows_preserve_large_integer_values() {
        let raw = r#"{"id":"10000000-0000-4000-8000-000000000001","tenant_id":"20000000-0000-4000-8000-000000000001","user_id":null,"entity_type":"CUSTOM","entity_id":null,"action":"UNKNOWN","before_state":{"n":18446744073709551615},"after_state":null,"ip_address":null,"user_agent":null,"created_at":"2026-10-19T00:00:00Z"}"#;
        let wire = serde_json::to_vec(&serde_json::json!({"records":[raw],"total":1})).unwrap();
        let page: RawHistoryPage = serde_json::from_slice(&wire).unwrap();
        assert_eq!(page.records[0], raw);
        let row: AuditRecordInput = serde_json::from_str(&page.records[0]).unwrap();
        assert_eq!(row.before_state.unwrap()["n"].as_u64(), Some(u64::MAX));
    }
    fn row(id: u128, tenant_id: Uuid) -> AuditRecordInput {
        AuditRecordInput {
            id: Uuid::from_u128(id),
            tenant_id,
            user_id: None,
            entity_type: "UNKNOWN".into(),
            entity_id: None,
            action: "UNKNOWN".into(),
            before_state: Some(serde_json::json!({"unknown":"é"})),
            after_state: None,
            ip_address: None,
            user_agent: None,
            created_at: chrono::DateTime::from_timestamp(100, 0).unwrap(),
        }
    }
    fn filter(tenant_id: Uuid) -> AuditHistoryFilter {
        AuditHistoryFilter {
            tenant_id,
            entity_type: None,
            entity_id: None,
            actions: Vec::new(),
            since: None,
            until: None,
            offset: 0,
            limit: 500,
        }
    }
    #[test]
    fn proofs_bind_domain_bytes_and_tenant_config() {
        let key = [7; 32];
        let p = sign(b"exact\n", &key, REQUEST_DOMAIN).unwrap();
        assert_eq!(verify(&p, &key, REQUEST_DOMAIN).unwrap(), b"exact\n");
        assert!(verify(&p, &key, RESPONSE_DOMAIN).is_err());
        assert!(verify(&p, &[8; 32], REQUEST_DOMAIN).is_err());
        let mut bad = p;
        bad.payload = URL_SAFE_NO_PAD.encode(b"different");
        assert!(verify(&bad, &key, REQUEST_DOMAIN).is_err());
    }
    #[test]
    fn merge_is_duplicate_safe_and_rejects_conflicts_and_cross_tenant() {
        let t = Uuid::new_v4();
        let mut m = BTreeMap::new();
        insert_record(&mut m, row(1, t)).unwrap();
        insert_record(&mut m, row(1, t)).unwrap();
        insert_record(&mut m, row(2, t)).unwrap();
        insert_record(&mut m, row(3, Uuid::new_v4())).unwrap();
        let mut conflict = row(1, t);
        conflict.action = "changed".into();
        assert!(insert_record(&mut m, conflict).is_err());
        let page = finish(m, &filter(t));
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, Uuid::from_u128(2));
    }
    #[test]
    fn configuration_rejects_untrusted_endpoint_and_short_key() {
        let t = Uuid::new_v4();
        for url in [
            "http://example.com/internal/private-events/history",
            "https://user@example.com/internal/private-events/history",
            "https://example.com/arbitrary",
            "https://example.com/internal/private-events/history?q=1",
        ] {
            let c = serde_json::json!({"schema_version":1,"bindings":[{"tenant_id":t,"url":url,"key_b64":STANDARD.encode([7;32])}]});
            assert!(binding(&serde_json::to_vec(&c).unwrap(), t).is_err());
        }
        let c = serde_json::json!({"schema_version":1,"bindings":[{"tenant_id":t,"url":"https://history.example/internal/private-events/history","key_b64":STANDARD.encode([7;32])}]});
        assert!(binding(&serde_json::to_vec(&c).unwrap(), t).is_ok());
        assert!(binding(&serde_json::to_vec(&c).unwrap(), Uuid::new_v4()).is_err());
        let mut short = c.clone();
        short["bindings"][0]["key_b64"] = STANDARD.encode([7; 16]).into();
        assert!(binding(&serde_json::to_vec(&short).unwrap(), t).is_err());
        let mut duplicate = c.clone();
        duplicate["bindings"]
            .as_array_mut()
            .unwrap()
            .push(c["bindings"][0].clone());
        assert!(binding(&serde_json::to_vec(&duplicate).unwrap(), t).is_err());
    }
}
