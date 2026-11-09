//! Tenant-scoped analytics and private delivery inspection (HR only at resolver).

use chrono::{DateTime, Utc};
use kabipay_common::private_events::{
    delivery_history_client::{
        self, decode_record, read_configured_delivery_history, DeliveryHistoryFilter,
    },
    DeliveryIntentInput,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::integration_connector as ic_connector;
use kabipay_db_entities::tenant::d0024_analytics::{
    dashboard, dashboard_widget, report_definition, report_schedule, workforce_snapshot,
};
use kabipay_db_entities::tenant::d0026_integrations::{tenant_integration, webhook_subscription};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

pub async fn list_report_definitions(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<report_definition::Model>> {
    let limit = limit.clamp(1, 200);
    report_definition::Entity::find()
        .filter(report_definition::Column::TenantId.eq(tenant_id))
        .order_by_asc(report_definition::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_report_schedules(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<report_schedule::Model>> {
    let limit = limit.clamp(1, 200);
    report_schedule::Entity::find()
        .filter(report_schedule::Column::TenantId.eq(tenant_id))
        .order_by_desc(report_schedule::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_dashboards(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<dashboard::Model>> {
    let limit = limit.clamp(1, 100);
    dashboard::Entity::find()
        .filter(dashboard::Column::TenantId.eq(tenant_id))
        .order_by_asc(dashboard::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_dashboard_widgets(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    dashboard_id: Option<Uuid>,
    limit: u64,
) -> KabiPayResult<Vec<dashboard_widget::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q =
        dashboard_widget::Entity::find().filter(dashboard_widget::Column::TenantId.eq(tenant_id));
    if let Some(did) = dashboard_id {
        q = q.filter(dashboard_widget::Column::DashboardId.eq(did));
    }
    q.order_by_asc(dashboard_widget::Column::GridRow)
        .order_by_asc(dashboard_widget::Column::GridCol)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_workforce_snapshots(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<workforce_snapshot::Model>> {
    let limit = limit.clamp(1, 120);
    workforce_snapshot::Entity::find()
        .filter(workforce_snapshot::Column::TenantId.eq(tenant_id))
        .order_by_desc(workforce_snapshot::Column::SnapshotDate)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboxHistoryRow {
    pub id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub event_type: String,
    pub payload: Value,
    pub status: String,
    pub retry_count: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct WebhookHistoryRow {
    pub id: Uuid,
    pub webhook_subscription_id: Uuid,
    pub event_name: Option<String>,
    pub payload_json: Option<Value>,
    pub http_status: Option<i32>,
    pub response_body: Option<String>,
    pub is_success: bool,
    pub attempt_number: i32,
    pub delivered_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
fn delivery_filter(allowed: Vec<&str>, limit: u64, mode: &str) -> DeliveryHistoryFilter {
    DeliveryHistoryFilter {
        mode: mode.into(),
        aggregates: allowed.into_iter().map(str::to_owned).collect(),
        event_ids: vec![],
        limit: limit.clamp(1, 500),
        before: None,
        scan_all: false,
    }
}
fn intent_event(intent: &DeliveryIntentInput) -> KabiPayResult<OutboxHistoryRow> {
    let body = intent.payload.get("body").unwrap_or(&intent.payload);
    let parse = |key: &str| -> KabiPayResult<Uuid> {
        body.get(key)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| KabiPayError::Internal("invalid historical webhook identity".into()))
    };
    if parse("outbox_id")? != intent.resource_id
        || parse("tenant_id")? != intent.tenant_id
        || body.get("event_type").and_then(Value::as_str) != Some(intent.event_type.as_str())
    {
        return Err(KabiPayError::Internal(
            "invalid webhook event binding".into(),
        ));
    }
    Ok(OutboxHistoryRow {
        id: intent.resource_id,
        aggregate_type: body
            .get("aggregate_type")
            .and_then(Value::as_str)
            .ok_or_else(|| KabiPayError::Internal("missing webhook aggregate".into()))?
            .into(),
        aggregate_id: parse("aggregate_id")?,
        event_type: intent.event_type.clone(),
        payload: body.get("payload").cloned().unwrap_or(Value::Null),
        status: "PENDING".into(),
        retry_count: 0,
        last_error: None,
        created_at: intent.occurred_at,
        processed_at: None,
        claimed_at: None,
    })
}
fn merge_event(
    events: &mut BTreeMap<Uuid, OutboxHistoryRow>,
    row: OutboxHistoryRow,
) -> KabiPayResult<()> {
    if let Some(prior) = events.get(&row.id) {
        if prior.aggregate_type != row.aggregate_type
            || prior.aggregate_id != row.aggregate_id
            || prior.event_type != row.event_type
            || prior.payload != row.payload
            || prior.created_at.timestamp_micros() != row.created_at.timestamp_micros()
        {
            return Err(KabiPayError::Conflict(
                "original event identity has conflicting history".into(),
            ));
        }
    }
    if let Some(prior) = events.get_mut(&row.id) {
        prior.retry_count = prior.retry_count.max(row.retry_count);
    } else {
        events.insert(row.id, row);
    }
    Ok(())
}
fn receipt_failure_count(status: &str, lease: i64) -> i32 {
    let active_or_success = matches!(
        status,
        "PROCESSING" | "DELIVERED" | "DISABLED" | "NO_DESTINATION"
    );
    lease
        .saturating_sub(i64::from(active_or_success))
        .clamp(0, i32::MAX as i64) as i32
}
fn requeued_event(
    intent: &DeliveryIntentInput,
    retry_count: i32,
) -> KabiPayResult<OutboxHistoryRow> {
    let mut event = intent_event(intent)?;
    event.retry_count = retry_count;
    Ok(event)
}
/// Raw archive rows retain their fields; compact source receipts own current outcomes.
pub async fn list_outbox_events(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: Option<String>,
    limit: u64,
    allowed_aggregates: Vec<&str>,
) -> KabiPayResult<Vec<OutboxHistoryRow>> {
    let mut filter = delivery_filter(allowed_aggregates.clone(), 500, "events");
    filter.scan_all = status.as_deref().is_some_and(|s| !s.trim().is_empty());
    let records = read_configured_delivery_history(db, tenant_id, &filter).await?;
    let mut events = BTreeMap::new();
    let mut destinations: BTreeMap<
        Uuid,
        Vec<(
            String,
            i64,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<String>,
        )>,
    > = BTreeMap::new();
    for r in records {
        let raw = decode_record(&r, tenant_id)?;
        match r.source_kind.as_str() {
            "legacy_outbox_event" => {
                let event: OutboxHistoryRow = serde_json::from_value(raw)?;
                if allowed_aggregates.contains(&event.aggregate_type.as_str()) {
                    merge_event(&mut events, event)?;
                }
            }
            "webhook" => {
                let intent: DeliveryIntentInput = serde_json::from_value(raw)?;
                let event = intent_event(&intent)?;
                if allowed_aggregates.contains(&event.aggregate_type.as_str()) {
                    merge_event(&mut events, event)?;
                    destinations.entry(intent.resource_id).or_default();
                }
            }
            _ => {}
        }
    }
    let ids: Vec<_> = events.keys().map(ToString::to_string).collect();
    if !ids.is_empty() {
        let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT resource_id,status,lease_generation,accepted_at,CASE WHEN status='PROCESSING' THEN updated_at ELSE NULL END AS claimed_at,outcome_code FROM private_delivery_receipt WHERE tenant_id=$1 AND source_kind='webhook' AND resource_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))",[tenant_id.into(),serde_json::to_value(ids)?.into()])).await?;
        for row in rows {
            destinations
                .entry(row.try_get("", "resource_id")?)
                .or_default()
                .push((
                    row.try_get("", "status")?,
                    row.try_get("", "lease_generation")?,
                    row.try_get("", "accepted_at")?,
                    row.try_get("", "claimed_at")?,
                    row.try_get("", "outcome_code")?,
                ));
        }
    }
    for (id, states) in destinations {
        if let Some(event) = events.get_mut(&id) {
            if states.is_empty() {
                return Err(KabiPayError::Internal(
                    "private delivery source receipt missing".into(),
                ));
            }
            event.status = if states.iter().any(|s| s.0 == "UNCERTAIN") {
                "UNCERTAIN"
            } else if states.iter().any(|s| s.0 == "PROCESSING") {
                "PROCESSING"
            } else if states
                .iter()
                .any(|s| matches!(s.0.as_str(), "RETRYABLE" | "PERMANENT"))
            {
                "FAILED"
            } else if states.iter().any(|s| s.0 == "PENDING") {
                "PENDING"
            } else if states.iter().all(|s| s.0 == "NO_DESTINATION") {
                "NO_DESTINATION"
            } else if states.iter().all(|s| s.0 == "DISABLED") {
                "DISABLED"
            } else {
                "PROCESSED"
            }
            .into();
            event.retry_count = event.retry_count.saturating_add(
                states
                    .iter()
                    .map(|s| receipt_failure_count(&s.0, s.1))
                    .max()
                    .unwrap_or(0),
            );
            event.processed_at = if event.status == "PROCESSED" {
                states.iter().filter_map(|s| s.2).max()
            } else {
                None
            };
            event.claimed_at = states.iter().filter_map(|s| s.3).min();
            event.last_error = states
                .iter()
                .filter(|s| matches!(s.0.as_str(), "RETRYABLE" | "PERMANENT" | "UNCERTAIN"))
                .filter_map(|s| s.4.clone())
                .next();
        }
    }
    let mut rows: Vec<_> = events
        .into_values()
        .filter(|r| {
            status
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none_or(|s| r.status == s)
        })
        .collect();
    rows.sort_by(|a, b| (b.created_at, b.id).cmp(&(a.created_at, a.id)));
    rows.truncate(limit.clamp(1, 500) as usize);
    Ok(rows)
}
pub async fn requeue_outbox_event(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
    actor: Uuid,
    allowed: Vec<&str>,
) -> KabiPayResult<OutboxHistoryRow> {
    let mut f = delivery_filter(allowed.clone(), 1, "events");
    f.event_ids.push(id);
    let records = read_configured_delivery_history(db, tenant_id, &f).await?;
    let mut intents = Vec::new();
    let mut legacy_retries = 0;
    for r in records {
        if r.source_kind == "legacy_outbox_event" {
            let original: OutboxHistoryRow = serde_json::from_value(decode_record(&r, tenant_id)?)?;
            if original.id == id && allowed.contains(&original.aggregate_type.as_str()) {
                legacy_retries = original.retry_count.max(0);
            }
        }
        if r.source_kind == "webhook" {
            let intent: DeliveryIntentInput =
                serde_json::from_value(decode_record(&r, tenant_id)?)?;
            if intent.resource_id == id
                && allowed.contains(&intent_event(&intent)?.aggregate_type.as_str())
            {
                intents.push(intent);
            }
        }
    }
    if intents.is_empty() {
        return Err(KabiPayError::NotFound {
            entity: "outbox_event",
            id: id.to_string(),
        });
    }
    let tx = db.begin().await?;
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1 FOR SHARE",
        [tenant_id.into()],
    ))
    .await?
    .ok_or_else(|| KabiPayError::Internal("delivery source authority missing".into()))?;
    let mut replayed = 0;
    let mut source_retries = 0;
    let receipts=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT intent_id,destination_id,status,lease_generation FROM private_delivery_receipt WHERE tenant_id=$1 AND resource_id=$2 AND source_kind='webhook' ORDER BY intent_id,destination_id FOR UPDATE",[tenant_id.into(),id.into()])).await?;
    for row in receipts {
        let state: String = row.try_get("", "status")?;
        source_retries = source_retries.max(receipt_failure_count(
            &state,
            row.try_get("", "lease_generation")?,
        ));
        if matches!(state.as_str(), "RETRYABLE" | "PERMANENT") {
            let intent_id: Uuid = row.try_get("", "intent_id")?;
            let destination: Uuid = row.try_get("", "destination_id")?;
            let intent = intents
                .iter()
                .find(|intent| intent.id == intent_id && intent.destination_id == destination)
                .ok_or_else(|| {
                    KabiPayError::Internal("failed destination intent history is incomplete".into())
                })?;
            delivery_history_client::append_delivery_replay(&tx, tenant_id, actor, intent).await?;
            replayed += 1;
        } else if matches!(state.as_str(), "UNCERTAIN" | "PROCESSING") {
            return Err(KabiPayError::Conflict(
                "active or uncertain delivery requires explicit reconciliation".into(),
            ));
        }
    }
    if replayed == 0 {
        return Err(KabiPayError::Validation(
            "event has no failed destinations to replay".into(),
        ));
    }
    tx.commit().await?;
    requeued_event(&intents[0], legacy_retries.saturating_add(source_retries))
}

// ---- Integrations (0026 — tenant) + global connector catalog (ops) + audit (0027) ----

pub async fn list_integration_connectors_global(
    ops_db: &DatabaseConnection,
    limit: u64,
) -> KabiPayResult<Vec<ic_connector::Model>> {
    let limit = limit.clamp(1, 200);
    ic_connector::Entity::find()
        .filter(ic_connector::Column::IsActive.eq(true))
        .order_by_asc(ic_connector::Column::Name)
        .limit(limit)
        .all(ops_db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_tenant_integrations(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<tenant_integration::Model>> {
    let limit = limit.clamp(1, 200);
    tenant_integration::Entity::find()
        .filter(tenant_integration::Column::TenantId.eq(tenant_id))
        .order_by_desc(tenant_integration::Column::ConnectedAt)
        .order_by_desc(tenant_integration::Column::UpdatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_webhook_subscriptions(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<webhook_subscription::Model>> {
    let limit = limit.clamp(1, 200);
    webhook_subscription::Entity::find()
        .filter(webhook_subscription::Column::TenantId.eq(tenant_id))
        .order_by_desc(webhook_subscription::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_webhook_delivery_logs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<WebhookHistoryRow>> {
    let records =
        read_configured_delivery_history(db, tenant_id, &delivery_filter(vec![], limit, "logs"))
            .await?;
    let mut rows = Vec::new();
    for r in records {
        let v = decode_record(&r, tenant_id)?;
        match r.source_kind.as_str() {
            "legacy_webhook_delivery_log" => {
                rows.push(serde_json::from_value::<WebhookHistoryRow>(v)?)
            }
            "delivery_outcome" => {
                let destination = v
                    .get("destination_id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or_else(|| KabiPayError::Internal("invalid delivery attempt".into()))?;
                let response = v.get("response");
                rows.push(WebhookHistoryRow {
                    id: r.record_id,
                    webhook_subscription_id: destination,
                    event_name: r.event_name,
                    payload_json: None,
                    http_status: None,
                    response_body: response
                        .and_then(|x| x.get("outcome_code"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    is_success: response
                        .and_then(|x| x.get("status"))
                        .and_then(Value::as_str)
                        == Some("DELIVERED"),
                    attempt_number: v
                        .get("lease_generation")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        .clamp(0, i32::MAX as i64) as i32,
                    delivered_at: r.occurred_at,
                    created_at: r.occurred_at,
                });
            }
            _ => {}
        }
    }
    rows.sort_by(|a, b| (b.delivered_at, b.id).cmp(&(a.delivered_at, a.id)));
    rows.truncate(limit.clamp(1, 500) as usize);
    Ok(rows)
}

pub async fn list_audit_logs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<kabipay_common::private_events::AuditRecordInput>> {
    let filter = kabipay_common::private_events::AuditHistoryFilter {
        tenant_id,
        entity_type: None,
        entity_id: None,
        actions: Vec::new(),
        since: None,
        until: None,
        offset: 0,
        limit: limit.clamp(1, 500),
    };
    Ok(
        kabipay_common::private_events::history_client::read_configured_audit_history(db, &filter)
            .await?,
    )
}

fn hash_optional_webhook_secret(secret: Option<&str>) -> Option<String> {
    secret.filter(|s| !s.trim().is_empty()).map(|s| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        format!("{:x}", h.finalize())
    })
}

pub async fn register_webhook_subscription(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    event_name: String,
    endpoint_url: String,
    webhook_secret_plain: Option<String>,
) -> KabiPayResult<webhook_subscription::Model> {
    let evt = event_name.trim();
    if evt.is_empty() {
        return Err(KabiPayError::Validation(
            "event_name must be non-empty".into(),
        ));
    }
    let url = endpoint_url.trim().to_string();
    if url.is_empty() || !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(KabiPayError::Validation(
            "endpoint_url must be an http(s) URL".into(),
        ));
    }
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let am = webhook_subscription::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        event_name: Set(evt.into()),
        endpoint_url: Set(url),
        secret_hash: Set(hash_optional_webhook_secret(
            webhook_secret_plain.as_deref(),
        )),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    webhook_subscription::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("webhook_subscription insert missing".into()))
}

pub async fn set_webhook_subscription_active(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
    active: bool,
) -> KabiPayResult<webhook_subscription::Model> {
    let m = webhook_subscription::Entity::find_by_id(id)
        .filter(webhook_subscription::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "webhook_subscription",
            id: id.to_string(),
        })?;
    let now = chrono::Utc::now();
    let mut am: webhook_subscription::ActiveModel = m.into();
    am.is_active = Set(active);
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    webhook_subscription::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("webhook_subscription missing".into()))
}

#[cfg(test)]
mod delivery_history_tests {
    use super::*;
    fn intent() -> DeliveryIntentInput {
        let id = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let occurred = Utc::now();
        DeliveryIntentInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            destination_id: Uuid::new_v4(),
            destination_version: 1,
            source_kind: "webhook".into(),
            resource_id: id,
            event_type: "expense.approved".into(),
            payload: serde_json::json!({"schema_version":1,"outbox_id":id,"tenant_id":tenant,"aggregate_type":"expense","aggregate_id":Uuid::new_v4(),"event_type":"expense.approved","payload":{"amount":"17.00"},"created_at":occurred}),
            occurred_at: occurred,
        }
    }
    #[test]
    fn event_merge_rejects_changed_original_payload_and_identity() {
        let input = intent();
        let row = intent_event(&input).unwrap();
        let mut events = BTreeMap::new();
        merge_event(&mut events, row.clone()).unwrap();
        merge_event(&mut events, row.clone()).unwrap();
        let mut conflict = row;
        conflict.payload = serde_json::json!({"amount":"999"});
        assert!(merge_event(&mut events, conflict).is_err());
    }
    #[test]
    fn webhook_original_event_binding_cannot_cross_tenant_or_resource() {
        let mut input = intent();
        input.payload["tenant_id"] = serde_json::json!(Uuid::new_v4());
        assert!(intent_event(&input).is_err());
        let mut input = intent();
        input.resource_id = Uuid::new_v4();
        assert!(intent_event(&input).is_err());
    }
    #[test]
    fn replay_result_preserves_original_id_and_retry_history() {
        let input = intent();
        let row = requeued_event(&input, 7).unwrap();
        assert_eq!(row.id, input.resource_id);
        assert_eq!(row.status, "PENDING");
        assert_eq!(row.retry_count, 7);
        assert!(row.processed_at.is_none());
        assert_eq!(row.payload, serde_json::json!({"amount":"17.00"}));
    }
    #[test]
    fn retry_counts_preserve_failures_without_counting_success_or_inflight_as_failure() {
        for (status, lease, expected) in [
            ("RETRYABLE", 1, 1),
            ("PERMANENT", 3, 3),
            ("UNCERTAIN", 2, 2),
            ("PENDING", 3, 3),
            ("PROCESSING", 2, 1),
            ("DELIVERED", 2, 1),
            ("NO_DESTINATION", 0, 0),
        ] {
            assert_eq!(receipt_failure_count(status, lease), expected);
        }
        let mut original = intent_event(&intent()).unwrap();
        original.retry_count = 5;
        let mut same = original.clone();
        same.retry_count = 0;
        let mut events = BTreeMap::new();
        merge_event(&mut events, same).unwrap();
        merge_event(&mut events, original.clone()).unwrap();
        assert_eq!(events[&original.id].retry_count, 5);
    }
}
