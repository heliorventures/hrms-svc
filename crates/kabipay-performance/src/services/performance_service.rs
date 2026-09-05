//! Tenant-scoped SeaORM queries for performance management.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0018_performance::{goal, review_cycle};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_cycles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<review_cycle::Model>> {
    let limit = limit.clamp(1, 40);
    review_cycle::Entity::find()
        .filter(review_cycle::Column::TenantId.eq(tenant_id))
        .order_by_desc(review_cycle::Column::StartDate)
        .order_by_asc(review_cycle::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_goals(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<goal::Model>> {
    let limit = limit.clamp(1, 200);
    goal::Entity::find()
        .filter(goal::Column::TenantId.eq(tenant_id))
        .order_by_desc(goal::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
