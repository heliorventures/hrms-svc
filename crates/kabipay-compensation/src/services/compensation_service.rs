//! Tenant-scoped SeaORM queries for compensation planning.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0021_compensation::{compensation_review_cycle, salary_band};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_bands(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<salary_band::Model>> {
    let limit = limit.clamp(1, 200);
    salary_band::Entity::find()
        .filter(salary_band::Column::TenantId.eq(tenant_id))
        .order_by_asc(salary_band::Column::Grade)
        .order_by_asc(salary_band::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_cycles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<compensation_review_cycle::Model>> {
    let limit = limit.clamp(1, 40);
    compensation_review_cycle::Entity::find()
        .filter(compensation_review_cycle::Column::TenantId.eq(tenant_id))
        .order_by_desc(compensation_review_cycle::Column::Year)
        .order_by_asc(compensation_review_cycle::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
