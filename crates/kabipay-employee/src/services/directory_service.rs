//! Tenant-scoped employee lookup used after the resolver authorizes the request.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

fn current_employee_query(tenant_id: Uuid) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.ne("TERMINATED"))
}

pub async fn find_current_by_id(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Option<employee::Model>> {
    current_employee_query(tenant_id)
        .filter(employee::Column::Id.eq(employee_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}
