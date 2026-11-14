//! Ops-plane SeaORM queries for billing.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::{billing_cycle, invoice, payment};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};
use uuid::Uuid;

pub async fn list_invoices(
    db: &DatabaseConnection,
    allowed_tenants: Option<&[Uuid]>,
    limit: u64,
) -> KabiPayResult<Vec<invoice::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = invoice::Entity::find().filter(invoice::Column::IsDeleted.eq(false));
    if let Some(tenants) = allowed_tenants {
        q = q.filter(invoice::Column::TenantId.is_in(tenants.iter().copied()));
    }
    q.order_by_desc(invoice::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_payments(
    db: &DatabaseConnection,
    invoice_id: Option<Uuid>,
    allowed_tenants: Option<&[Uuid]>,
    limit: u64,
) -> KabiPayResult<Vec<payment::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = payment::Entity::find();
    if let Some(inv) = invoice_id {
        q = q.filter(payment::Column::InvoiceId.eq(inv));
    }
    if let Some(tenants) = allowed_tenants {
        let authorized_invoices = invoice::Entity::find()
            .select_only()
            .column(invoice::Column::Id)
            .filter(invoice::Column::TenantId.is_in(tenants.iter().copied()))
            .filter(invoice::Column::IsDeleted.eq(false));
        q = q.filter(payment::Column::InvoiceId.in_subquery(authorized_invoices.into_query()));
    }
    q.order_by_desc(payment::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_billing_cycles(
    db: &DatabaseConnection,
    allowed_tenants: Option<&[Uuid]>,
    limit: u64,
) -> KabiPayResult<Vec<billing_cycle::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = billing_cycle::Entity::find();
    if let Some(tenants) = allowed_tenants {
        q = q.filter(billing_cycle::Column::TenantId.is_in(tenants.iter().copied()));
    }
    q.order_by_desc(billing_cycle::Column::PeriodStart)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
