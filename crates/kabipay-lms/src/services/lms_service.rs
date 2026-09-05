//! Tenant-scoped SeaORM queries for LMS catalogue.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0019_lms::{course, skill};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

pub async fn list_skills(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<skill::Model>> {
    let limit = limit.clamp(1, 200);
    skill::Entity::find()
        .filter(skill::Column::TenantId.eq(tenant_id))
        .order_by_asc(skill::Column::Name)
        .order_by_asc(skill::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_courses(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    active_only: bool,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<course::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q = course::Entity::find().filter(course::Column::TenantId.eq(tenant_id));
    if active_only {
        q = q.filter(course::Column::IsActive.eq(true));
    }
    q.order_by_asc(course::Column::Title)
        .order_by_asc(course::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}
