//! Tenant-scoped SeaORM queries for recruitment.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0016_recruitment::{application, job_posting};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_jobs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<job_posting::Model>> {
    let limit = limit.clamp(1, 200);
    job_posting::Entity::find()
        .filter(job_posting::Column::TenantId.eq(tenant_id))
        .order_by_desc(job_posting::Column::CreatedAt)
        .order_by_asc(job_posting::Column::Id)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_applications(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<application::Model>> {
    let limit = limit.clamp(1, 500);
    application::Entity::find()
        .filter(application::Column::TenantId.eq(tenant_id))
        .filter(application::Column::IsDeleted.eq(false))
        .order_by_desc(application::Column::AppliedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
