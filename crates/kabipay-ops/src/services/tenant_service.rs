//! Ops-plane SeaORM queries for tenants, modules, subscriptions.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::{module, tenant, tenant_subscription};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_tenants(
    db: &DatabaseConnection,
    allowed_tenants: Option<&[Uuid]>,
    limit: u64,
) -> KabiPayResult<Vec<tenant::Model>> {
    let limit = limit.clamp(1, 500);
    let mut query = tenant::Entity::find().filter(tenant::Column::IsDeleted.eq(false));
    if let Some(tenants) = allowed_tenants {
        query = query.filter(tenant::Column::Id.is_in(tenants.iter().copied()));
    }
    query.order_by_asc(tenant::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_modules(
    db: &DatabaseConnection,
    limit: u64,
    include_inactive: bool,
) -> KabiPayResult<Vec<module::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = module::Entity::find();
    if !include_inactive {
        q = q.filter(module::Column::IsActive.eq(true));
    }
    q.order_by_asc(module::Column::DisplayOrder)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_subscriptions(
    db: &DatabaseConnection,
    allowed_tenants: Option<&[Uuid]>,
    limit: u64,
) -> KabiPayResult<Vec<tenant_subscription::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = tenant_subscription::Entity::find()
        .filter(tenant_subscription::Column::IsDeleted.eq(false));
    if let Some(tenants) = allowed_tenants {
        q = q.filter(tenant_subscription::Column::TenantId.is_in(tenants.iter().copied()));
    }
    q.order_by_desc(tenant_subscription::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
