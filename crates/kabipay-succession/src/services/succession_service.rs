//! Tenant-scoped SeaORM queries for succession planning.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0020_succession::{competency, talent_pool};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_competencies(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<competency::Model>> {
    let limit = limit.clamp(1, 200);
    competency::Entity::find()
        .filter(competency::Column::TenantId.eq(tenant_id))
        .order_by_asc(competency::Column::Name)
        .order_by_asc(competency::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_pools(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<talent_pool::Model>> {
    let limit = limit.clamp(1, 100);
    talent_pool::Entity::find()
        .filter(talent_pool::Column::TenantId.eq(tenant_id))
        .order_by_asc(talent_pool::Column::Name)
        .order_by_asc(talent_pool::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
