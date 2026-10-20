//! Configuration activity is separate from company business history and diagnostics.
use crate::{
    execution::Execution,
    identity::SourceIdentity,
    policy::MANAGE,
    store::*,
    workspace::{limit_page, source_member},
};
use chrono::{DateTime, Duration, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
const QUERY:&str="SELECT event_id,event_type,source_tenant_id,actor_principal_id,aggregate_id,toString(aggregate_version) AS aggregate_version,toString(configuration_version) AS configuration_version,formatDateTime(occurred_at,'%Y-%m-%dT%H:%i:%S.%fZ','UTC') AS occurred_at_utc,actor FROM organization.control_history FINAL WHERE organization_id={organization:UUID} AND occurred_at>={from:DateTime64(3,'UTC')} AND occurred_at<={to:DateTime64(3,'UTC')} AND event_id>{after:UUID} ORDER BY event_id LIMIT {limit:UInt16} FORMAT JSONEachRow";
const KINDS: &[&str] = &[
    "organization.bootstrapped",
    "organization.configuration_changed",
    "relationship.proposed",
    "relationship.manager_changed",
    "relationship.ended",
    "relationship.installed",
    "grant.installed",
    "membership.installed",
    "directory.synchronized",
    "command.cancelled",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditActor {
    principal_id: Uuid,
    source_tenant_id: Uuid,
    source_user_id: Uuid,
    issuer: String,
}
fn bad() -> KabiPayError {
    KabiPayError::Internal("organization activity is unavailable".into())
}
fn document(row: Value, from: DateTime<Utc>, to: DateTime<Utc>) -> KabiPayResult<Value> {
    let uuid = |k: &str| -> KabiPayResult<Uuid> {
        row[k]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .filter(|v| !v.is_nil())
            .ok_or_else(bad)
    };
    let version = |k: &str| -> KabiPayResult<String> {
        let s = row[k].as_str().ok_or_else(bad)?;
        let n = s.parse::<u64>().map_err(|_| bad())?;
        if n == 0 || n.to_string() != s {
            return Err(bad());
        }
        Ok(s.into())
    };
    let id = uuid("event_id")?;
    let tenant = uuid("source_tenant_id")?;
    let principal = uuid("actor_principal_id")?;
    let aggregate = uuid("aggregate_id")?;
    let kind = row["event_type"]
        .as_str()
        .filter(|s| KINDS.contains(s))
        .ok_or_else(bad)?;
    let occurred = DateTime::parse_from_rfc3339(row["occurred_at_utc"].as_str().ok_or_else(bad)?)
        .map_err(|_| bad())?
        .with_timezone(&Utc);
    if occurred < from || occurred > to {
        return Err(bad());
    }
    let actor: AuditActor =
        serde_json::from_str(row["actor"].as_str().ok_or_else(bad)?).map_err(|_| bad())?;
    if actor.principal_id != principal
        || actor.source_tenant_id != tenant
        || actor.source_user_id.is_nil()
        || actor.issuer != "kabipay-client"
    {
        return Err(bad());
    }
    Ok(
        json!({"id":id,"event_type":kind,"source_tenant_id":tenant,"actor_principal_id":principal,"aggregate_id":aggregate,"aggregate_version":version("aggregate_version")?,"configuration_version":version("configuration_version")?,"occurred_at":occurred}),
    )
}
impl Execution {
    #[allow(clippy::too_many_arguments)] // Explicit bounded history cursor contract.
    pub async fn audit_history(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<Value> {
        valid_id(org)?;
        limit_page(after, limit)?;
        if to < from || to - from > Duration::days(31) {
            return Err(KabiPayError::Validation(
                "activity range must be ordered and at most31days".into(),
            ));
        }
        let tx = self.store.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let identity = p.identity_version;
        let link = p.link_version;
        tx.commit().await?;
        let client = self.reporting.as_ref().ok_or_else(bad)?;
        let rows = client
            .query(
                QUERY,
                &[
                    ("organization", org.to_string()),
                    ("from", from.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                    ("to", to.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                    ("after", after.unwrap_or(Uuid::nil()).to_string()),
                    ("limit", (limit + 1).to_string()),
                ],
            )
            .await?;
        if rows.len() > limit as usize + 1 {
            return Err(bad());
        }
        let more = rows.len() > limit as usize;
        let mut items = Vec::new();
        let mut last = after.unwrap_or(Uuid::nil());
        for row in rows.into_iter().take(limit as usize) {
            let item = document(row, from, to)?;
            let id = Uuid::parse_str(item["id"].as_str().ok_or_else(bad)?).map_err(|_| bad())?;
            if id <= last {
                return Err(bad());
            }
            last = id;
            items.push(item);
        }
        let check = self.store.db.begin().await?;
        let now = current(&check, actor).await?;
        if now.id != p.id || now.identity_version != identity || now.link_version != link {
            return Err(denied());
        }
        source_member(&check, actor, org).await?;
        authorized(&check, actor, p.id, org, MANAGE).await?;
        check.commit().await?;
        Ok(
            json!({"schema_version":1,"status":"OBSERVED_ACTIVITY","coverage":"NOT_ATTESTED","items":items,"has_next_page":more,"end_cursor":if more{Some(last)}else{None}}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audit_query_never_selects_raw_change_or_global_identity_events() {
        assert!(!QUERY.contains(",change"));
        assert!(QUERY.contains("WHERE organization_id={organization:UUID}"));
        assert!(!KINDS.contains(&"identity.registered"));
    }
}
