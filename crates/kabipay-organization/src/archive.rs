//! Fixed archive sidecar; encryption keys and source database credentials never enter this service.
use crate::{
    execution::Execution,
    identity::SourceIdentity,
    reporting::{
        bounded_file, companies, coverage, history_bounds, history_document, reauthorize,
        reporting_url, unavailable,
    },
    store::{current, sql, valid_id},
};
use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::CompanyEvent;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
// Sidecar plaintext is capped at16MiB; allow its bounded per-event manifest
// overhead in transit without accepting an unbounded response.
const MAX_BYTES: usize = 20 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct ArchiveClient {
    url: reqwest::Url,
    http: reqwest::Client,
}
impl ArchiveClient {
    pub(crate) fn from_env() -> KabiPayResult<Option<Self>> {
        let raw = match std::env::var("KABIPAY_ORGANIZATION_ARCHIVE_URL") {
            Ok(s) => s,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(unavailable()),
        };
        let url = reporting_url(&raw)?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(30));
        if let Ok(path) = std::env::var("KABIPAY_ORGANIZATION_ARCHIVE_CA_FILE") {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&bounded_file(&path, 1024 * 1024)?)
                    .map_err(|_| unavailable())?,
            );
        }
        match (
            std::env::var("KABIPAY_ORGANIZATION_ARCHIVE_CLIENT_CERT_FILE"),
            std::env::var("KABIPAY_ORGANIZATION_ARCHIVE_CLIENT_KEY_FILE"),
        ) {
            (Ok(cert), Ok(key)) => {
                let mut pem = bounded_file(&cert, 1024 * 1024)?;
                pem.push(b'\n');
                pem.extend_from_slice(&bounded_file(&key, 1024 * 1024)?);
                builder =
                    builder.identity(reqwest::Identity::from_pem(&pem).map_err(|_| unavailable())?);
            }
            (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent))
                if url.scheme() == "http" => {}
            _ => {
                return Err(KabiPayError::Internal(
                    "archive HTTPS requires a configured client certificate and key".into(),
                ))
            }
        }
        Ok(Some(Self {
            url,
            http: builder.build().map_err(|_| unavailable())?,
        }))
    }
    async fn request(
        &self,
        engine: &Execution,
        tenant: Uuid,
        route: &str,
        request: Value,
    ) -> KabiPayResult<ArchiveResponse> {
        let body = serde_json::to_vec(&request)?;
        let proof = engine
            .service_issuer
            .issue("kabipay-organization-archive", tenant, route, &body)
            .map_err(|_| unavailable())?;
        let mut url = self.url.clone();
        url.set_path(route);
        let mut response = self
            .http
            .post(url)
            .bearer_auth(proof)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| unavailable())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveResponse {
    manifest: Manifest,
    events: Vec<CompanyEvent>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    id: Uuid,
    organization_id: Uuid,
    source_tenant_id: Uuid,
    membership_id: Uuid,
    membership_version: i64,
    history_from: DateTime<Utc>,
    history_through: DateTime<Utc>,
    row_count: usize,
    plaintext_sha256: String,
    ciphertext_sha256: String,
    key_version: String,
    events: Vec<EventHash>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventHash {
    event_id: Uuid,
    sha256: String,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[allow(clippy::too_many_arguments)]
fn output(
    response: ArchiveResponse,
    org: Uuid,
    tenant: Uuid,
    member: Uuid,
    version: i64,
    id: Uuid,
    range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    name: &str,
) -> KabiPayResult<Value> {
    let m = response.manifest;
    if m.schema_version != 1
        || m.id != id
        || m.organization_id != org
        || m.source_tenant_id != tenant
        || m.membership_id != member
        || m.membership_version != version
        || m.row_count > 10000
        || m.row_count != response.events.len()
        || m.row_count != m.events.len()
        || !hash(&m.plaintext_sha256)
        || !hash(&m.ciphertext_sha256)
        || m.key_version.is_empty()
        || m.key_version.len() > 128
        || m.history_from > m.history_through
        || range.is_some_and(|r| r != (m.history_from, m.history_through))
    {
        return Err(unavailable());
    }
    let hashes: BTreeMap<_, _> = m
        .events
        .iter()
        .map(|e| (e.event_id, e.sha256.as_str()))
        .collect();
    if hashes.len() != m.row_count || hashes.iter().any(|(id, s)| id.is_nil() || !hash(s)) {
        return Err(unavailable());
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::with_capacity(m.row_count);
    let names = BTreeMap::from([(tenant, name)]);
    for e in response.events {
        if e.schema_version != 1
            || e.organization_id != org
            || e.source_tenant_id != tenant
            || !hashes.contains_key(&e.event_id)
            || !seen.insert(e.event_id)
            || e.occurred_at < m.history_from
            || e.occurred_at > m.history_through
        {
            return Err(unavailable());
        }
        items.push(history_document(json!({"event_id":e.event_id,"event_type":e.event_type,"source_tenant_id":e.source_tenant_id,"aggregate_type":e.aggregate_type,"aggregate_id":e.aggregate_id,"aggregate_version":e.aggregate_version.to_string(),"occurred_at":e.occurred_at,"deleted":u8::from(e.deleted),"payload":serde_json::to_string(&e.payload)?,"actor":serde_json::to_string(&e.actor)?}),&names)?);
    }
    Ok(
        json!({"schema_version":1,"manifest":{"schema_version":m.schema_version,"id":m.id,"organization_id":m.organization_id,"source_tenant_id":m.source_tenant_id,"membership_id":m.membership_id,"membership_version":m.membership_version,"history_from":m.history_from,"history_through":m.history_through,"row_count":m.row_count,"plaintext_sha256":m.plaintext_sha256,"ciphertext_sha256":m.ciphertext_sha256,"key_version":m.key_version},"items":items}),
    )
}
impl Execution {
    #[allow(clippy::too_many_arguments)]
    pub async fn history_archive(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
        id: Uuid,
        range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    ) -> KabiPayResult<Value> {
        valid_id(id)?;
        if let Some((from, through)) = range {
            history_bounds(from, through)?;
            if through > Utc::now() {
                return Err(KabiPayError::Validation(
                    "archive range cannot extend into the future".into(),
                ));
            }
        }
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        let tx = self.store.db.begin().await?;
        let selected = companies(&tx, actor, org, Some(vec![member]), "history:read").await?;
        reauthorize(&tx, actor, org, &selected, "history:export").await?;
        let company = selected.first().ok_or_else(unavailable)?;
        let p = current(&tx, actor).await?;
        let current_version:i64=tx.query_one(sql("SELECT version FROM kabipay_ops.organization_company WHERE id=$1 AND organization_id=$2",vec![member.into(),org.into()])).await?.ok_or_else(unavailable)?.try_get("","version")?;
        let version = if range.is_some() {
            current_version
        } else {
            tx.query_one(sql("SELECT membership_version FROM kabipay_ops.organization_archive_manifest WHERE id=$1 AND organization_id=$2 AND source_tenant_id=$3 AND membership_id=$4 AND state='VERIFIED'",vec![id.into(),org.into(),company.tenant.into(),member.into()])).await?.ok_or_else(unavailable)?.try_get("","membership_version")?
        };
        if let Some((from, _)) = range {
            if coverage(&tx, org, company.tenant, from).await?["state"] != "READY" {
                return Err(KabiPayError::Conflict(
                    "HISTORY_INCOMPLETE: current capture coverage required".into(),
                ));
            }
        }
        let mut request = json!({"archive_id":id,"organization_id":org,"source_tenant_id":company.tenant,"membership_id":member,"membership_version":version,"actor":{"principal_id":p.id,"source_tenant_id":actor.tenant(),"source_user_id":actor.user()}});
        if let Some((from, through)) = range {
            request["from"] = json!(from);
            request["through"] = json!(through);
        }
        let route = if range.is_some() {
            "/v1/archive/export"
        } else {
            "/v1/archive/retrieve"
        };
        let response = self
            .archive
            .as_ref()
            .ok_or_else(unavailable)?
            .request(self, company.tenant, route, request)
            .await?;
        let result = output(
            response,
            org,
            company.tenant,
            member,
            version,
            id,
            range,
            &company.name,
        )?;
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        reauthorize(&tx, actor, org, &selected, "history:read").await?;
        reauthorize(&tx, actor, org, &selected, "history:export").await?;
        let observed: i64 = tx
            .query_one(sql(
                "SELECT version FROM kabipay_ops.organization_company WHERE id=$1",
                vec![member.into()],
            ))
            .await?
            .ok_or_else(unavailable)?
            .try_get("", "version")?;
        if observed != current_version {
            return Err(crate::store::denied());
        }
        tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty(org: Uuid, tenant: Uuid, member: Uuid, id: Uuid) -> ArchiveResponse {
        ArchiveResponse {
            manifest: Manifest {
                schema_version: 1,
                id,
                organization_id: org,
                source_tenant_id: tenant,
                membership_id: member,
                membership_version: 1,
                history_from: Utc::now(),
                history_through: Utc::now(),
                row_count: 0,
                plaintext_sha256: "a".repeat(64),
                ciphertext_sha256: "b".repeat(64),
                key_version: "v1".into(),
                events: vec![],
            },
            events: vec![],
        }
    }
    #[test]
    fn archives_bind_exact_scope_count_and_range() {
        let (org, tenant, member, id) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        assert!(output(
            empty(org, tenant, member, id),
            org,
            tenant,
            member,
            1,
            id,
            None,
            "Company"
        )
        .is_ok());
        assert!(output(
            empty(org, tenant, member, id),
            org,
            Uuid::new_v4(),
            member,
            1,
            id,
            None,
            "Company"
        )
        .is_err());
        assert!(output(
            empty(org, tenant, member, id),
            org,
            tenant,
            member,
            2,
            id,
            None,
            "Company"
        )
        .is_err());
        let mut r = empty(org, tenant, member, id);
        r.manifest.row_count = 1;
        assert!(output(r, org, tenant, member, 1, id, None, "Company").is_err());
        let now = Utc::now();
        assert!(output(
            empty(org, tenant, member, id),
            org,
            tenant,
            member,
            1,
            id,
            Some((now, now)),
            "Company"
        )
        .is_err());
    }
}
