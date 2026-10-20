//! Purpose-limited reporting; current policy and explicit capture coverage gate reads.
use crate::{
    execution::Execution,
    identity::SourceIdentity,
    runtime_policy::resolve_authority,
    store::{current, denied, sql, valid_id},
    workspace::{limit_page, page, source_member, OrganizationDocumentPage},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::CompanyEventActor;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};
use uuid::Uuid;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const PROGRESS_SQL: &str = "SELECT source_tenant_id,work_date,toString(sum(toUInt128(logged_seconds))) AS logged_seconds,toString(sum(toUInt128(approved_seconds))) AS approved_seconds FROM organization.progress_daily WHERE organization_id={organization:UUID} AND source_tenant_id IN {tenants:Array(UUID)} AND work_date>={from:Date} AND work_date<={to:Date} GROUP BY source_tenant_id,work_date ORDER BY source_tenant_id,work_date FORMAT JSONEachRow";
const HISTORY_SQL: &str = "SELECT event_id,event_type,source_tenant_id,aggregate_type,aggregate_id,toString(aggregate_version) AS aggregate_version,formatDateTime(occurred_at,'%Y-%m-%dT%H:%i:%S.%fZ','UTC') AS occurred_at_utc,deleted,payload,actor FROM organization.business_history FINAL WHERE organization_id={organization:UUID} AND source_tenant_id IN {tenants:Array(UUID)} AND occurred_at>={from:DateTime64(3,'UTC')} AND occurred_at<={to:DateTime64(3,'UTC')} AND event_id>{after:UUID} ORDER BY event_id LIMIT {limit:UInt16} FORMAT JSONEachRow";

fn invalid_config() -> KabiPayError {
    KabiPayError::Internal("invalid organization reporting configuration".into())
}
pub(crate) fn unavailable() -> KabiPayError {
    KabiPayError::Internal("HISTORY_UNAVAILABLE".into())
}
fn incomplete() -> KabiPayError {
    KabiPayError::Conflict("HISTORY_INCOMPLETE: accepted retained coverage is required".into())
}

#[derive(Clone)]
pub struct ReportingClient {
    url: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    http: reqwest::Client,
}
pub(crate) fn reporting_url(value: &str) -> KabiPayResult<reqwest::Url> {
    if value.len() > 2048 {
        return Err(invalid_config());
    }
    let url = reqwest::Url::parse(value).map_err(|_| invalid_config())?;
    let loopback = url.host_str().is_some_and(|h| {
        h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid_config());
    }
    Ok(url)
}
pub(crate) fn bounded_file(path: &str, max: u64) -> KabiPayResult<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| invalid_config())?;
    let metadata = file.metadata().map_err(|_| invalid_config())?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(invalid_config());
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_config())?;
    if bytes.len() as u64 > max {
        return Err(invalid_config());
    }
    Ok(bytes)
}
impl ReportingClient {
    pub fn from_env() -> KabiPayResult<Option<Self>> {
        let url = match std::env::var("KABIPAY_ORGANIZATION_CLICKHOUSE_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => {
                if std::env::var_os("KABIPAY_ORGANIZATION_CLICKHOUSE_USER").is_some()
                    || std::env::var_os("KABIPAY_ORGANIZATION_CLICKHOUSE_PASSWORD_FILE").is_some()
                {
                    return Err(invalid_config());
                }
                return Ok(None);
            }
            Err(_) => return Err(invalid_config()),
        };
        let url = reporting_url(&url)?;
        let user =
            std::env::var("KABIPAY_ORGANIZATION_CLICKHOUSE_USER").map_err(|_| invalid_config())?;
        if user.is_empty() || user.len() > 128 {
            return Err(invalid_config());
        }
        let secret_path = std::env::var("KABIPAY_ORGANIZATION_CLICKHOUSE_PASSWORD_FILE")
            .map_err(|_| invalid_config())?;
        let secret =
            String::from_utf8(bounded_file(&secret_path, 4096)?).map_err(|_| invalid_config())?;
        let secret = secret.trim_end_matches(['\r', '\n']);
        if secret.is_empty() {
            return Err(invalid_config());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-clickhouse-user",
            reqwest::header::HeaderValue::from_str(&user).map_err(|_| invalid_config())?,
        );
        let mut value =
            reqwest::header::HeaderValue::from_str(secret).map_err(|_| invalid_config())?;
        value.set_sensitive(true);
        headers.insert("x-clickhouse-key", value);
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(8));
        if let Some(path) = std::env::var_os("KABIPAY_ORGANIZATION_CLICKHOUSE_CA_FILE") {
            let path = path.to_str().ok_or_else(invalid_config)?;
            let certificate = reqwest::Certificate::from_pem(&bounded_file(path, 1024 * 1024)?)
                .map_err(|_| invalid_config())?;
            builder = builder.add_root_certificate(certificate);
        }
        Ok(Some(Self {
            url,
            headers,
            http: builder.build().map_err(|_| invalid_config())?,
        }))
    }
    pub(crate) async fn query(
        &self,
        query: &str,
        parameters: &[(&str, String)],
    ) -> KabiPayResult<Vec<Value>> {
        let mut url = self.url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("query", query);
            for (key, value) in parameters {
                pairs.append_pair(&format!("param_{key}"), value);
            }
        }
        let mut response = self
            .http
            .post(url)
            .headers(self.headers.clone())
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body(Vec::new())
            .send()
            .await
            .map_err(|_| {
                tracing::warn!(event_code = "ORGANIZATION_REPORTING_TRANSPORT_FAILED");
                unavailable()
            })?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            tracing::warn!(
                event_code = "ORGANIZATION_REPORTING_RESPONSE_REJECTED",
                http_status = response.status().as_u16()
            );
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|_| {
                    tracing::warn!(event_code = "ORGANIZATION_REPORTING_JSON_REJECTED");
                    unavailable()
                })
            })
            .collect()
    }
}

pub(crate) struct Company {
    pub(crate) membership: Uuid,
    pub(crate) tenant: Uuid,
    pub(crate) name: String,
}
fn member_selection(ids: Option<Vec<Uuid>>) -> KabiPayResult<Option<BTreeSet<Uuid>>> {
    ids.map(|ids| {
        if ids.is_empty() || ids.len() > 100 || ids.iter().any(Uuid::is_nil) {
            return Err(KabiPayError::Validation(
                "1..100 unique company memberships required".into(),
            ));
        }
        let set: BTreeSet<_> = ids.iter().copied().collect();
        if set.len() != ids.len() {
            return Err(KabiPayError::Validation(
                "company memberships must be unique".into(),
            ));
        }
        Ok(set)
    })
    .transpose()
}
pub(crate) fn progress_bounds(from: NaiveDate, to: NaiveDate) -> KabiPayResult<()> {
    let days = to.signed_duration_since(from).num_days();
    if !(0..=365).contains(&days) {
        return Err(KabiPayError::Validation(
            "progress range must be ordered and at most 366 days".into(),
        ));
    }
    Ok(())
}
pub(crate) fn history_bounds(from: DateTime<Utc>, to: DateTime<Utc>) -> KabiPayResult<()> {
    if to < from || to.signed_duration_since(from) > Duration::days(31) {
        return Err(KabiPayError::Validation(
            "history range must be ordered and at most 31 days".into(),
        ));
    }
    Ok(())
}
pub(crate) async fn companies(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    ids: Option<Vec<Uuid>>,
    capability: &str,
) -> KabiPayResult<Vec<Company>> {
    valid_id(org)?;
    let requested = member_selection(ids)?;
    let principal = current(tx, actor).await?;
    source_member(tx, actor, org).await?;
    let rows=tx.query_all(sql("SELECT m.id,m.tenant_id,t.name FROM kabipay_ops.organization_company m JOIN kabipay_ops.organization o ON o.id=m.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE m.organization_id=$1 AND o.status='ACTIVE' AND m.status='ACTIVE' AND m.installed_version=m.version AND m.runtime_status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND t.status='ACTIVE' AND NOT t.is_deleted ORDER BY m.id LIMIT 101",vec![org.into()])).await?;
    if rows.len() > 100 {
        return Err(KabiPayError::Validation(
            "organization company limit exceeded".into(),
        ));
    }
    let mut result = Vec::new();
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        if requested.as_ref().is_some_and(|set| !set.contains(&id)) {
            continue;
        }
        match resolve_authority(tx, principal.id, org, id, capability, None, true).await {
            Ok(_) => result.push(Company {
                membership: id,
                tenant: row.try_get("", "tenant_id")?,
                name: row.try_get("", "name")?,
            }),
            Err(KabiPayError::Forbidden(_)) if requested.is_none() => {}
            Err(error) => return Err(error),
        }
    }
    if result.is_empty()
        || requested
            .as_ref()
            .is_some_and(|set| set.len() != result.len())
    {
        return Err(denied());
    }
    Ok(result)
}
pub(crate) async fn reauthorize(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    companies: &[Company],
    capability: &str,
) -> KabiPayResult<()> {
    let principal = current(tx, actor).await?;
    source_member(tx, actor, org).await?;
    for company in companies {
        resolve_authority(
            tx,
            principal.id,
            org,
            company.membership,
            capability,
            None,
            true,
        )
        .await?;
    }
    Ok(())
}
fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|v| v.with_timezone(&Utc))
}
#[cfg(test)] // Legacy watermark characterization; runtime uses the end-to-end marker barrier below.
fn freshness(capture: Option<&Value>, now: DateTime<Utc>, from: DateTime<Utc>) -> Value {
    let Some(capture) = capture else {
        return json!({"state":"PARTIAL","reason_code":"CAPTURE_NOT_ACCEPTED","projected_at":null});
    };
    let expected = capture["expected_partitions"].as_array();
    if capture["acceptance_status"] != "ACCEPTED"
        || capture["bootstrap_complete"] != true
        || capture["schema_version"] != 1
        || expected.is_none_or(|rows| rows.is_empty())
        || timestamp(&capture["history_from"]).is_none_or(|start| from < start)
    {
        return json!({"state":"PARTIAL","reason_code":"BOOTSTRAP_COVERAGE_INCOMPLETE","projected_at":null});
    }
    let mut oldest_projected: Option<DateTime<Utc>> = None;
    let mut oldest_source: Option<DateTime<Utc>> = None;
    for partition in expected.into_iter().flatten() {
        let matches: Vec<_> = capture["watermarks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["partition_id"] == *partition && row["topic"] == capture["topic"])
            .collect();
        if matches.len() != 1 || matches[0]["state"] != "READY" {
            return json!({"state":"PARTIAL","reason_code":"PARTITION_COVERAGE_GAP","projected_at":null});
        }
        let Some(projected) = timestamp(&matches[0]["projected_at"]) else {
            return json!({"state":"PARTIAL","reason_code":"PARTITION_COVERAGE_GAP","projected_at":null});
        };
        let Some(source) = timestamp(&matches[0]["source_observed_at"]) else {
            return json!({"state":"STALE","reason_code":"NO_SOURCE_FRESHNESS_PROOF","projected_at":projected});
        };
        oldest_projected = Some(oldest_projected.map_or(projected, |v| v.min(projected)));
        oldest_source = Some(oldest_source.map_or(source, |v| v.min(source)));
    }
    let fresh = oldest_projected
        .zip(oldest_source)
        .is_some_and(|(projected, source)| {
            [projected, source]
                .iter()
                .all(|time| (-60..=300).contains(&now.signed_duration_since(*time).num_seconds()))
        });
    json!({"state":if fresh{"READY"}else{"STALE"},"reason_code":if fresh{"ACCEPTED_PARTITIONS_CURRENT"}else{"EVENT_WATERMARK_STALE"},"projected_at":oldest_projected,"source_observed_at":oldest_source,"capture_status":capture["acceptance_status"],"expected_partitions":capture["expected_partitions"]})
}
pub(crate) async fn coverage(
    tx: &DatabaseTransaction,
    org: Uuid,
    tenant: Uuid,
    from: DateTime<Utc>,
) -> KabiPayResult<Value> {
    let row=tx.query_one(sql("SELECT jsonb_build_object('acceptance_status',c.acceptance_status,'bootstrap_complete',c.bootstrap_complete,'schema_version',c.schema_version,'expected_partitions',to_jsonb(c.expected_partitions),'history_from',c.history_from,'source_bootstrap_through',c.source_bootstrap_through,'healthy',kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']),'source_observed_at',(SELECT max(marker.source_marker_at) FROM kabipay_ops.organization_capture_marker marker WHERE marker.organization_id=c.organization_id AND marker.source_tenant_id=c.source_tenant_id)) AS data FROM kabipay_ops.organization_company_capture c JOIN kabipay_ops.organization_company m ON m.id=c.membership_id AND m.organization_id=c.organization_id AND m.tenant_id=c.source_tenant_id AND m.version=c.membership_version WHERE c.organization_id=$1 AND c.source_tenant_id=$2",vec![org.into(),tenant.into()])).await?;
    let data: Option<Value> = row.map(|r| r.try_get("", "data")).transpose()?;
    Ok(capture_coverage(data.as_ref(), from))
}
fn capture_coverage(data: Option<&Value>, from: DateTime<Utc>) -> Value {
    let Some(data) = data else {
        return json!({"state":"PARTIAL","reason_code":"CAPTURE_NOT_ACCEPTED"});
    };
    let covered = data["acceptance_status"] == "ACCEPTED"
        && data["bootstrap_complete"] == true
        && data["schema_version"] == 1
        && timestamp(&data["history_from"]).is_some_and(|start| from >= start);
    json!({"state":if !covered{"PARTIAL"}else if data["healthy"]==true{"READY"}else{"STALE"},"reason_code":if !covered{"BOOTSTRAP_COVERAGE_INCOMPLETE"}else if data["healthy"]==true{"SOURCE_CDC_ALL_CONSUMERS_CURRENT"}else{"CAPTURE_BARRIER_INCOMPLETE_OR_STALE"},"source_observed_at":data["source_observed_at"],"history_from":data["history_from"],"expected_partitions":data["expected_partitions"]})
}
pub(crate) fn tenants(companies: &[Company]) -> String {
    format!(
        "[{}]",
        companies
            .iter()
            .map(|company| format!("'{}'", company.tenant))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn safe_seconds(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|v| *v <= MAX_SAFE_INTEGER)
}
fn exact_seconds(value: &str) -> Option<u128> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
#[derive(Default)]
struct ProgressTotals {
    logged: u128,
    approved: u128,
    daily: BTreeMap<NaiveDate, (u128, u128)>,
    invalid: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressRow {
    source_tenant_id: Uuid,
    work_date: NaiveDate,
    logged_seconds: String,
    approved_seconds: String,
}

impl Execution {
    pub async fn progress(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> KabiPayResult<Value> {
        progress_bounds(from, to)?;
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        let tx = self.store.db.begin().await?;
        let companies = companies(&tx, actor, org, membership_ids, "analytics:read").await?;
        let from_time = from.and_hms_opt(0, 0, 0).ok_or_else(unavailable)?.and_utc();
        let mut states = BTreeMap::new();
        for company in &companies {
            states.insert(
                company.tenant,
                coverage(&tx, org, company.tenant, from_time).await?,
            );
        }
        let mut totals: BTreeMap<Uuid, ProgressTotals> = BTreeMap::new();
        let mut sink_available = false;
        if let Some(client) = &self.reporting {
            if let Ok(rows) = client
                .query(
                    PROGRESS_SQL,
                    &[
                        ("organization", org.to_string()),
                        ("tenants", tenants(&companies)),
                        ("from", from.to_string()),
                        ("to", to.to_string()),
                    ],
                )
                .await
            {
                sink_available = true;
                for row in rows {
                    let Ok(row) = serde_json::from_value::<ProgressRow>(row) else {
                        sink_available = false;
                        break;
                    };
                    if !states.contains_key(&row.source_tenant_id)
                        || row.work_date < from
                        || row.work_date > to
                    {
                        sink_available = false;
                        break;
                    }
                    let company = totals.entry(row.source_tenant_id).or_default();
                    let Some(values) = exact_seconds(&row.logged_seconds)
                        .zip(exact_seconds(&row.approved_seconds))
                    else {
                        company.invalid = true;
                        continue;
                    };
                    if company.daily.insert(row.work_date, values).is_some() {
                        sink_available = false;
                        break;
                    }
                    if let Some(total) = company
                        .logged
                        .checked_add(values.0)
                        .zip(company.approved.checked_add(values.1))
                    {
                        company.logged = total.0;
                        company.approved = total.1;
                    } else {
                        company.invalid = true;
                    }
                }
            }
        }
        let mut partial = false;
        let mut result = Vec::with_capacity(companies.len());
        for company in &companies {
            let mut state = states.remove(&company.tenant).ok_or_else(unavailable)?;
            if !sink_available {
                state["state"] = json!("UNAVAILABLE");
                state["reason_code"] = json!("REPORTING_SINK_UNAVAILABLE");
            } else if totals
                .get(&company.tenant)
                .is_some_and(|totals| totals.invalid)
            {
                state["state"] = json!("UNAVAILABLE");
                state["reason_code"] = json!("REPORTING_VALUE_OUT_OF_RANGE");
            }
            let complete = sink_available && state["state"] == "READY";
            partial |= !complete;
            let (logged, approved, daily) = if complete {
                let values = totals.get(&company.tenant);
                (Some(values.map_or(0,|v|v.logged).to_string()),Some(values.map_or(0,|v|v.approved).to_string()),Some(values.into_iter().flat_map(|v|v.daily.iter()).map(|(date,(logged,approved))|json!({"work_date":date,"logged_seconds":logged.to_string(),"approved_seconds":approved.to_string()})).collect::<Vec<_>>()))
            } else {
                (None, None, None)
            };
            result.push(json!({"membership_id":company.membership,"tenant_id":company.tenant,"company_name":company.name,"logged_seconds":logged,"approved_seconds":approved,"daily":daily,"freshness":state}));
        }
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        reauthorize(&tx, actor, org, &companies, "analytics:read").await?;
        for company in &companies {
            let state = coverage(&tx, org, company.tenant, from_time).await?;
            if state["state"] != "READY" {
                for row in &mut result {
                    if row["membership_id"] == company.membership.to_string() {
                        row["freshness"] = state.clone();
                        for key in ["logged_seconds", "approved_seconds", "daily"] {
                            row[key] = Value::Null;
                        }
                        partial = true;
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(json!({"schema_version":1,"companies":result,"partial":partial}))
    }
    #[allow(clippy::too_many_arguments)] // Matches the public scoped/date/cursor reporting interface.
    pub async fn history(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        after: Option<Uuid>,
        first: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        history_bounds(from, to)?;
        limit_page(after, first)?;
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        let tx = self.store.db.begin().await?;
        let companies = companies(&tx, actor, org, membership_ids, "history:read").await?;
        let client = self.reporting.as_ref().ok_or_else(unavailable)?;
        for company in &companies {
            if coverage(&tx, org, company.tenant, from).await?["state"] != "READY" {
                return Err(incomplete());
            }
        }
        let rows = client
            .query(
                HISTORY_SQL,
                &[
                    ("organization", org.to_string()),
                    ("tenants", tenants(&companies)),
                    ("from", from.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                    ("to", to.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                    ("after", after.unwrap_or_else(Uuid::nil).to_string()),
                    ("limit", (first + 1).to_string()),
                ],
            )
            .await?;
        if rows.len() > usize::try_from(first + 1).map_err(|_| unavailable())? {
            return Err(unavailable());
        }
        let names: BTreeMap<_, _> = companies
            .iter()
            .map(|c| (c.tenant, c.name.as_str()))
            .collect();
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(history_document(row, &names)?);
        }
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        reauthorize(&tx, actor, org, &companies, "history:read").await?;
        for company in &companies {
            if coverage(&tx, org, company.tenant, from).await?["state"] != "READY" {
                return Err(incomplete());
            }
        }
        tx.commit().await?;
        page(result, first, 1)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRow {
    event_id: Uuid,
    event_type: String,
    source_tenant_id: Uuid,
    aggregate_type: String,
    aggregate_id: Uuid,
    aggregate_version: String,
    #[serde(rename = "occurred_at_utc", alias = "occurred_at")]
    occurred_at: DateTime<Utc>,
    deleted: u8,
    payload: String,
    #[serde(default)]
    actor: Option<String>,
}
fn history_actor(raw: Option<&str>, source_tenant: Uuid) -> KabiPayResult<Value> {
    let actor = match raw {
        None => None,
        Some(raw) if raw.len() <= 4096 => {
            serde_json::from_str::<Option<CompanyEventActor>>(raw).map_err(|_| unavailable())?
        }
        Some(_) => return Err(unavailable()),
    };
    let Some(actor) = actor else {
        return Ok(json!({"kind":"UNKNOWN"}));
    };
    match &actor {
        CompanyEventActor::OrganizationPrincipal {
            principal_id,
            display_name,
            identity_version,
            source_tenant_id,
            source_user_id,
            authority_id,
            authority_version,
            command_id,
            ..
        } => {
            for id in [
                principal_id,
                source_tenant_id,
                source_user_id,
                authority_id,
                command_id,
            ] {
                valid_id(*id).map_err(|_| unavailable())?;
            }
            if display_name.chars().count() > 255
                || *identity_version <= 0
                || *authority_version <= 0
                || u64::try_from(*identity_version)
                    .ok()
                    .is_none_or(|value| value > MAX_SAFE_INTEGER)
                || u64::try_from(*authority_version)
                    .ok()
                    .is_none_or(|value| value > MAX_SAFE_INTEGER)
            {
                return Err(unavailable());
            }
        }
        CompanyEventActor::LocalUser {
            local_user_id,
            source_tenant_id,
        } => {
            valid_id(*local_user_id).map_err(|_| unavailable())?;
            if *source_tenant_id != source_tenant {
                return Err(unavailable());
            }
        }
        CompanyEventActor::System => {}
    }
    serde_json::to_value(actor).map_err(|_| unavailable())
}
pub(crate) fn history_document(row: Value, names: &BTreeMap<Uuid, &str>) -> KabiPayResult<Value> {
    let row: HistoryRow = serde_json::from_value(row).map_err(|_| unavailable())?;
    let actor = history_actor(row.actor.as_deref(), row.source_tenant_id)?;
    let name = names.get(&row.source_tenant_id).ok_or_else(unavailable)?;
    let version = safe_seconds(&row.aggregate_version)
        .filter(|v| *v > 0)
        .ok_or_else(unavailable)?;
    if row.deleted > 1
        || row.event_id.is_nil()
        || row.aggregate_id.is_nil()
        || row.payload.len() > 65536
    {
        return Err(unavailable());
    }
    let payload: Value = serde_json::from_str(&row.payload).map_err(|_| unavailable())?;
    let allowed: &[&str] = match row.event_type.as_str() {
        "approval.changed" => &[
            "resource_type",
            "resource_id",
            "employee_id",
            "employee_display_name",
            "status",
            "summary",
        ],
        "timesheet.changed" => &[
            "employee_id",
            "work_date",
            "logged_seconds",
            "approved_seconds",
        ],
        "directory.changed" => &["employee_id", "display_name", "employment_status"],
        "attendance.changed" => &[
            "employee_id",
            "work_date",
            "status",
            "observed_seconds",
            "overtime_seconds",
            "late_minutes",
            "early_exit_minutes",
        ],
        "leave.changed" => &[
            "employee_id",
            "from_date",
            "to_date",
            "requested_centidays",
            "status",
        ],
        "project_time.changed" => &[
            "employee_id",
            "work_date",
            "project_code",
            "logged_seconds",
            "approved_seconds",
        ],
        "administration.changed" => &["resource_type", "resource_id", "action", "status"],
        "legacy_audit.recorded" => &[
            "source_audit_id",
            "source_entity_type",
            "source_entity_id",
            "source_action",
            "source_user_id",
            "before_status",
            "after_status",
            "source_sha256",
        ],
        _ => return Err(unavailable()),
    };
    let object = payload.as_object().ok_or_else(unavailable)?;
    if object.len() != allowed.len() || !object.keys().all(|key| allowed.contains(&key.as_str())) {
        return Err(unavailable());
    }
    if crate::extended_history::validate(
        &row.event_type,
        &row.aggregate_type,
        row.aggregate_id,
        version,
        row.deleted == 1,
        &payload,
    )? {
        return Ok(
            json!({"id":row.event_id,"event_type":row.event_type,"source_tenant_id":row.source_tenant_id,"company_name":name,"occurred_at":row.occurred_at,"aggregate_type":row.aggregate_type,"aggregate_id":row.aggregate_id,"aggregate_version":version,"deleted":row.deleted==1,"actor":actor,"payload":payload}),
        );
    }
    let employee = payload["employee_id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(unavailable)?;
    let text = |key: &str, max: usize| {
        payload[key]
            .as_str()
            .is_some_and(|value| value.chars().count() <= max)
    };
    match row.event_type.as_str() {
        "approval.changed" => {
            if !["leave_request", "timesheet", "performance_participant"]
                .contains(&row.aggregate_type.as_str())
                || payload["resource_type"] != row.aggregate_type
                || payload["resource_id"]
                    .as_str()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    != Some(row.aggregate_id)
                || !text("employee_display_name", 255)
                || !text("status", 120)
                || !text("summary", 240)
            {
                return Err(unavailable());
            }
        }
        "timesheet.changed" => {
            if row.aggregate_type != "timesheet_entry"
                || payload["work_date"]
                    .as_str()
                    .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                    .is_none()
                || !["logged_seconds", "approved_seconds"].iter().all(|key| {
                    payload[*key]
                        .as_u64()
                        .is_some_and(|value| value <= MAX_SAFE_INTEGER)
                })
            {
                return Err(unavailable());
            }
        }
        "directory.changed" => {
            if row.aggregate_type != "employee"
                || employee != row.aggregate_id
                || !text("display_name", 255)
                || !text("employment_status", 40)
            {
                return Err(unavailable());
            }
        }
        _ => return Err(unavailable()),
    }
    Ok(
        json!({"id":row.event_id,"event_type":row.event_type,"source_tenant_id":row.source_tenant_id,"company_name":name,"occurred_at":row.occurred_at,"aggregate_type":row.aggregate_type,"aggregate_id":row.aggregate_id,"aggregate_version":version,"deleted":row.deleted==1,"actor":actor,"payload":payload}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn clickhouse_post_declares_an_empty_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n < 8192);
                bytes.extend_from_slice(&chunk[..n]);
            }
            let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            assert!(headers.contains("content-length: 0\r\n"));
            let body = "{\"ok\":true}\n";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = ReportingClient {
            url: reporting_url(&format!("http://{address}/")).unwrap(),
            headers: reqwest::header::HeaderMap::new(),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap(),
        };
        assert_eq!(
            client.query("SELECT 1", &[]).await.unwrap(),
            vec![json!({"ok":true})]
        );
        server.await.unwrap();
    }
    #[test]
    fn idle_capture_requires_real_end_to_end_proof_and_retained_range() {
        let from = Utc::now() - Duration::days(1);
        let mut data = json!({"acceptance_status":"ACCEPTED","bootstrap_complete":true,"schema_version":1,"history_from":from-Duration::days(1),"healthy":false});
        assert_eq!(capture_coverage(Some(&data), from)["state"], "STALE");
        data["healthy"] = json!(true);
        assert_eq!(capture_coverage(Some(&data), from)["state"], "READY");
        assert_eq!(
            capture_coverage(Some(&data), from - Duration::days(2))["state"],
            "PARTIAL"
        );
        data["bootstrap_complete"] = json!(false);
        assert_eq!(capture_coverage(Some(&data), from)["state"], "PARTIAL");
    }
    #[test]
    fn fixed_reporting_transport_and_checked_numbers() {
        for url in [
            "http://remote.invalid/",
            "https://u:p@ch.invalid/",
            "https://ch.invalid/?query=SELECT+1",
            "https://ch.invalid/path",
        ] {
            assert!(reporting_url(url).is_err());
        }
        assert!(reporting_url("https://clickhouse:8443/").is_ok());
        assert!(reporting_url("http://127.0.0.1:18123/").is_ok());
        assert_eq!(safe_seconds("9007199254740991"), Some(MAX_SAFE_INTEGER));
        assert_eq!(safe_seconds("9007199254740992"), None);
        assert_eq!(safe_seconds("-1"), None);
        assert_eq!(
            exact_seconds("9007199254740993").map(|value| value.to_string()),
            Some("9007199254740993".into())
        );
        assert_eq!(
            exact_seconds("0000").map(|value| value.to_string()),
            Some("0".into())
        );
        assert_eq!(exact_seconds("-1"), None);
        assert_eq!(exact_seconds("+1"), None);
        assert_eq!(
            exact_seconds("340282366920938463463374607431768211456"),
            None
        );
    }
    #[test]
    fn unknown_coverage_is_partial_and_history_is_separately_bounded() {
        let now = Utc::now();
        assert_eq!(freshness(None, now, now)["state"], "PARTIAL");
        assert!(history_bounds(now, now + Duration::days(32)).is_err());
        assert!(history_bounds(now, now - Duration::seconds(1)).is_err());
        assert!(member_selection(Some(vec![Uuid::new_v4(); 2])).is_err());
        assert!(member_selection(Some(vec![])).is_err());
    }
    #[test]
    fn correction_selection_precedes_date_filter_and_history_is_deduplicated() {
        assert!(PROGRESS_SQL.contains("organization.progress_daily"));
        assert!(!PROGRESS_SQL.contains("business_history"));
        assert!(PROGRESS_SQL.contains("toUInt128"));
        assert!(HISTORY_SQL.contains("business_history FINAL"));
        assert!(HISTORY_SQL.contains("ORDER BY event_id LIMIT"));
        assert!(HISTORY_SQL.contains("{tenants:Array(UUID)}"));
    }
    #[test]
    fn no_event_heartbeat_does_not_prove_source_freshness() {
        let now = Utc::now();
        let capture = json!({"acceptance_status":"ACCEPTED","bootstrap_complete":true,"schema_version":1,"history_from":now-Duration::days(1),"topic":"fixed","expected_partitions":[0],"watermarks":[{"topic":"fixed","partition_id":0,"state":"READY","projected_at":now,"source_observed_at":now-Duration::hours(1)}]});
        assert_eq!(freshness(Some(&capture), now, now)["state"], "STALE");
        let mut gap = capture;
        gap["expected_partitions"] = json!([0, 1]);
        assert_eq!(freshness(Some(&gap), now, now)["state"], "PARTIAL");
    }
    #[test]
    fn private_or_nested_history_payloads_are_rejected() {
        let tenant = Uuid::new_v4();
        let aggregate = Uuid::new_v4();
        let names = BTreeMap::from([(tenant, "Synthetic company")]);
        let payload = json!({"resource_type":"leave_request","resource_id":aggregate,"employee_id":Uuid::new_v4(),"employee_display_name":"Synthetic","status":"PENDING","summary":"Leave request"});
        let mut row = json!({"event_id":Uuid::new_v4(),"event_type":"approval.changed","source_tenant_id":tenant,"aggregate_type":"leave_request","aggregate_id":aggregate,"aggregate_version":"2","occurred_at":Utc::now(),"deleted":0,"payload":payload.to_string()});
        assert!(history_document(row.clone(), &names).is_ok());
        let mut nested = payload.clone();
        nested["summary"] = json!({"medical_reason":"private"});
        row["payload"] = json!(nested.to_string());
        assert!(history_document(row.clone(), &names).is_err());
        let mut extra = payload;
        extra["private_review_body"] = json!("private");
        row["payload"] = json!(extra.to_string());
        assert!(history_document(row, &names).is_err());
    }
    #[test]
    fn history_actor_is_typed_and_legacy_attribution_remains_unknown() {
        let source = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        assert_eq!(
            history_actor(None, source).expect("legacy actor")["kind"],
            "UNKNOWN"
        );
        assert_eq!(
            history_actor(Some("null"), source).expect("null legacy actor")["kind"],
            "UNKNOWN"
        );
        assert_eq!(
            history_actor(Some(r#"{"kind":"system"}"#), source).expect("system actor")["kind"],
            "system"
        );
        let local =
            json!({"kind":"local_user","local_user_id":Uuid::new_v4(),"source_tenant_id":source});
        assert!(history_actor(Some(&local.to_string()), source).is_ok());
        assert!(history_actor(Some(&local.to_string()), foreign).is_err());
        let actor = json!({"kind":"organization_principal","principal_id":Uuid::new_v4(),"display_name":"अ".repeat(255),"identity_version":1,"source_tenant_id":foreign,"source_user_id":Uuid::new_v4(),"authority_kind":"organization_grant","authority_id":Uuid::new_v4(),"authority_version":2,"command_id":Uuid::new_v4()});
        assert!(history_actor(Some(&actor.to_string()), source).is_ok());
        let mut private = actor;
        private["jwt"] = json!("private");
        assert!(history_actor(Some(&private.to_string()), source).is_err());
    }
    #[tokio::test]
    async fn reporting_http_rejects_redirects_and_oversized_responses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for response in [
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n"
                .to_owned(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_RESPONSE_BYTES + 1
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("synthetic listener");
            let address = listener.local_addr().expect("listener address");
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("synthetic request");
                let mut request = [0; 4096];
                let read = stream.read(&mut request).await.expect("request read");
                assert!(read > 0, "synthetic HTTP request must be present");
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("response write");
            });
            let client = ReportingClient {
                url: reporting_url(&format!("http://{address}/")).expect("loopback url"),
                headers: reqwest::header::HeaderMap::new(),
                http: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()
                    .expect("synthetic HTTP client"),
            };
            assert!(client
                .query("SELECT {id:UUID}", &[("id", Uuid::new_v4().to_string())])
                .await
                .is_err());
            server.await.expect("synthetic server");
        }
    }
    #[tokio::test]
    #[ignore = "requires explicit disposable synthetic PostgreSQL acceptance"]
    async fn actual_postgres_company_reporting_authority() {
        assert_eq!(
            std::env::var("ORGANIZATION_SYNTHETIC_ACCEPTANCE").as_deref(),
            Ok("true")
        );
        let admin = sea_orm::Database::connect(
            "postgres://acceptance:synthetic-only-password@127.0.0.1:15436/organization_acceptance",
        )
        .await
        .expect("synthetic admin");
        let database = format!("organization_reporting_{}", Uuid::new_v4().simple());
        admin
            .execute_unprepared(&format!("CREATE DATABASE {database}"))
            .await
            .expect("disposable database");
        let db = sea_orm::Database::connect(format!(
            "postgres://acceptance:synthetic-only-password@127.0.0.1:15436/{database}"
        ))
        .await
        .expect("synthetic reporting database");
        let (
            org,
            source,
            target,
            other,
            source_member_id,
            target_member,
            other_member,
            principal,
            user,
            role,
            binding,
        ) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        db.execute_unprepared(&format!(r#"
CREATE SCHEMA kabipay_ops;
CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY,name text,status text,is_deleted boolean DEFAULT false);
CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY,status text);
CREATE TABLE kabipay_ops.organization_company(id uuid PRIMARY KEY,organization_id uuid,tenant_id uuid,status text,effective_from timestamptz DEFAULT now(),effective_to timestamptz,version bigint DEFAULT 1,installed_version bigint DEFAULT 1,runtime_status text DEFAULT 'ACTIVE');
CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY,identity_version bigint,display_name text,status text);
CREATE TABLE kabipay_ops.principal_login_link(id uuid PRIMARY KEY,principal_id uuid,source_tenant_id uuid,source_user_id uuid,issuer text,status text,version bigint);
CREATE TABLE kabipay_ops.organization_role(id uuid PRIMARY KEY,organization_id uuid,status text);
CREATE TABLE kabipay_ops.organization_role_capability(role_id uuid,organization_id uuid,capability_code text,employee_scope text);
CREATE TABLE kabipay_ops.principal_role_binding(id uuid PRIMARY KEY,organization_id uuid,principal_id uuid,role_id uuid,status text,effective_from timestamptz DEFAULT now(),effective_to timestamptz,version bigint);
CREATE TABLE kabipay_ops.role_binding_company(binding_id uuid,organization_id uuid,organization_company_id uuid,activation_status text,expected_grant_version bigint);
INSERT INTO kabipay_ops.tenant(id,name,status) VALUES('{source}','Source','ACTIVE'),('{target}','One','ACTIVE'),('{other}','Two','ACTIVE');
INSERT INTO kabipay_ops.organization VALUES('{org}','ACTIVE');
INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id,status) VALUES('{source_member_id}','{org}','{source}','ACTIVE'),('{target_member}','{org}','{target}','ACTIVE'),('{other_member}','{org}','{other}','ACTIVE');
INSERT INTO kabipay_ops.principal VALUES('{principal}',1,'Synthetic actor','ACTIVE');
INSERT INTO kabipay_ops.principal_login_link VALUES('{principal}','{principal}','{source}','{user}','kabipay-client','VERIFIED',1);
INSERT INTO kabipay_ops.organization_role VALUES('{role}','{org}','ACTIVE');
INSERT INTO kabipay_ops.organization_role_capability VALUES('{role}','{org}','analytics:read','ALL'),('{role}','{org}','organization:manage','ALL');
INSERT INTO kabipay_ops.principal_role_binding(id,organization_id,principal_id,role_id,status,version) VALUES('{binding}','{org}','{principal}','{role}','ACTIVE',1);
INSERT INTO kabipay_ops.role_binding_company VALUES('{binding}','{org}','{target_member}','ACTIVE',1);
"#)).await.expect("synthetic metadata fixture");
        let actor = SourceIdentity::eligible_source(source, user, "Synthetic actor".into());
        let tx = db.begin().await.expect("policy transaction");
        let visible = companies(&tx, &actor, org, None, "analytics:read")
            .await
            .expect("authorized target");
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].membership, target_member);
        assert!(companies(
            &tx,
            &actor,
            org,
            Some(vec![target_member, other_member]),
            "analytics:read"
        )
        .await
        .is_err());
        assert!(
            companies(&tx, &actor, org, Some(vec![target_member]), "history:read")
                .await
                .is_err()
        );
        tx.rollback().await.expect("release policy locks");
        for (change,reset) in [
            ("UPDATE kabipay_ops.organization_role_capability SET employee_scope='SELF' WHERE capability_code='analytics:read'","UPDATE kabipay_ops.organization_role_capability SET employee_scope='ALL' WHERE capability_code='analytics:read'"),
            ("UPDATE kabipay_ops.role_binding_company SET activation_status='PENDING'","UPDATE kabipay_ops.role_binding_company SET activation_status='ACTIVE'"),
            ("UPDATE kabipay_ops.principal_login_link SET status='REVOKED'","UPDATE kabipay_ops.principal_login_link SET status='VERIFIED'"),
            ("UPDATE kabipay_ops.organization_company SET status='SUSPENDED'","UPDATE kabipay_ops.organization_company SET status='ACTIVE'")
        ] {
            db.execute_unprepared(change).await.expect("policy change");let tx=db.begin().await.expect("changed policy transaction");assert!(companies(&tx,&actor,org,Some(vec![target_member]),"analytics:read").await.is_err());tx.rollback().await.expect("release changed policy locks");db.execute_unprepared(reset).await.expect("reset synthetic policy");
        }
        db.close().await.expect("close disposable pool");
        admin
            .execute_unprepared(&format!("DROP DATABASE {database}"))
            .await
            .expect("remove disposable database");
        admin.close().await.expect("close synthetic admin");
    }
}
