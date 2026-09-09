//! Tenant-scoped reads for analytics domain (0024) and outbox listing (0030, HR only at resolver).

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::integration_connector as ic_connector;
use kabipay_db_entities::tenant::d0026_integrations::{
    tenant_integration, webhook_delivery_log, webhook_subscription,
};
use kabipay_db_entities::tenant::d0027_communication_audit::audit_log;
use kabipay_db_entities::tenant::d0024_analytics::{
    dashboard, dashboard_widget, report_definition, report_schedule, workforce_snapshot,
};
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
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
    let mut q = dashboard_widget::Entity::find()
        .filter(dashboard_widget::Column::TenantId.eq(tenant_id));
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

pub async fn list_outbox_events(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: Option<String>,
    limit: u64,
    allowed_aggregates: Vec<&str>,
) -> KabiPayResult<Vec<outbox_event::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = outbox_event::Entity::find().filter(outbox_event::Column::TenantId.eq(tenant_id))
        .filter(outbox_event::Column::AggregateType.is_in(allowed_aggregates));
    if let Some(s) = status {
        let t = s.trim();
        if !t.is_empty() {
            q = q.filter(outbox_event::Column::Status.eq(t));
        }
    }
    q.order_by_desc(outbox_event::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

const OB_PENDING: &str = "PENDING";
const OB_FAILED: &str = "FAILED";
const OB_PROCESSING: &str = "PROCESSING";

/// HR: send a **FAILED** or stuck **PROCESSING** row back to **PENDING** for the worker to pick up.
pub async fn requeue_outbox_event(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> KabiPayResult<outbox_event::Model> {
    let m = outbox_event::Entity::find_by_id(id)
        .filter(outbox_event::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "outbox_event",
            id: id.to_string(),
        })?;
    if m.status != OB_FAILED && m.status != OB_PROCESSING {
        return Err(KabiPayError::Validation(
            "only FAILED or PROCESSING outbox events can be requeued".into(),
        ));
    }
    let note = " [manual requeue]";
    let prev_err = m.last_error.clone();
    let err = prev_err
        .map(|e| {
            let s = format!("{e}{note}");
            if s.len() > 2000 {
                format!("{}…", &s[..1997])
            } else {
                s
            }
        })
        .unwrap_or_else(|| "manual requeue".to_string());
    let mut am: outbox_event::ActiveModel = m.into();
    am.status = Set(OB_PENDING.into());
    am.processed_at = Set(None);
    am.claimed_at = Set(None);
    am.last_error = Set(Some(err));
    am.update(db).await?;
    outbox_event::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("outbox row missing after requeue".into()))
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
) -> KabiPayResult<Vec<webhook_delivery_log::Model>> {
    let limit = limit.clamp(1, 500);
    webhook_delivery_log::Entity::find()
        .filter(webhook_delivery_log::Column::TenantId.eq(tenant_id))
        .order_by_desc(webhook_delivery_log::Column::DeliveredAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_audit_logs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<audit_log::Model>> {
    let limit = limit.clamp(1, 500);
    audit_log::Entity::find()
        .filter(audit_log::Column::TenantId.eq(tenant_id))
        .order_by_desc(audit_log::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

fn hash_optional_webhook_secret(secret: Option<&str>) -> Option<String> {
    secret.filter(|s| !s.trim().is_empty()).map(|s| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        format!("{:x}", h.finalize())
    })
}

pub async fn connect_tenant_integration(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    connector_id: Uuid,
) -> KabiPayResult<tenant_integration::Model> {
    let now = chrono::Utc::now();
    if let Some(ex) = tenant_integration::Entity::find()
        .filter(tenant_integration::Column::TenantId.eq(tenant_id))
        .filter(tenant_integration::Column::IntegrationConnectorId.eq(connector_id))
        .one(db)
        .await?
    {
        let row_id = ex.id;
        let mut am: tenant_integration::ActiveModel = ex.into();
        am.is_active = Set(true);
        am.connected_at = Set(Some(now));
        am.updated_at = Set(now);
        am.update(db).await.map_err(KabiPayError::from)?;
        return tenant_integration::Entity::find_by_id(row_id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("tenant_integration race".into()));
    }
    let id = Uuid::new_v4();
    let am = tenant_integration::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        integration_connector_id: Set(connector_id),
        credentials_encrypted: Set(None),
        config_json: Set(None),
        is_active: Set(true),
        connected_at: Set(Some(now)),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    tenant_integration::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("tenant_integration insert missing".into()))
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
        secret_hash: Set(hash_optional_webhook_secret(webhook_secret_plain.as_deref())),
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
