//! Read-only access + HR resolution for `document_type`, `employee_document`.

use chrono::Utc;
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::entities::d0008_document_system::{document_type, employee_document};
use crate::entities::d0029_file_storage::file_storage;

pub async fn list_document_types(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<document_type::Model>> {
    let limit = limit.clamp(1, 200);
    document_type::Entity::find()
        .filter(document_type::Column::TenantId.eq(tenant_id))
        .filter(document_type::Column::IsDeleted.eq(false))
        .order_by_asc(document_type::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_employee_documents(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<employee_document::Model>> {
    let limit = limit.clamp(1, 200);
    employee_document::Entity::find()
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .filter(employee_document::Column::EmployeeId.eq(employee_id))
        .filter(employee_document::Column::IsDeleted.eq(false))
        .order_by_desc(employee_document::Column::UpdatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Find one employee document only when its owner is inside the resolver-supplied scope.
/// The service owns tenant and soft-delete predicates; callers own permission policy.
pub async fn find_employee_document_in_scope(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
    scope: &EmployeeScopeFilter,
) -> KabiPayResult<Option<employee_document::Model>> {
    let mut query = employee_document::Entity::find_by_id(document_id)
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .filter(employee_document::Column::IsDeleted.eq(false));
    match scope {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => return Ok(None),
        EmployeeScopeFilter::EmployeeIds(employee_ids) if employee_ids.is_empty() => {
            return Ok(None);
        }
        EmployeeScopeFilter::EmployeeIds(employee_ids) => {
            query = query.filter(
                employee_document::Column::EmployeeId.is_in(employee_ids.iter().copied()),
            );
        }
    }
    query.one(db).await.map_err(KabiPayError::from)
}

/// HR approval workflow: `PENDING` → `APPROVED` or `REJECTED`.
pub async fn resolve_employee_document_status(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
    approved: bool,
    verifier_user_id: Uuid,
) -> KabiPayResult<employee_document::Model> {
    let row = employee_document::Entity::find_by_id(document_id)
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .filter(employee_document::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee_document",
            id: document_id.to_string(),
        })?;
    if row.status != "PENDING" {
        return Err(KabiPayError::Validation(
            "only documents in PENDING status can be approved or rejected".into(),
        ));
    }
    let status = if approved { "APPROVED" } else { "REJECTED" };
    let now = Utc::now();
    let mut am: employee_document::ActiveModel = row.into();
    am.status = Set(status.into());
    am.verified_by = Set(Some(verifier_user_id));
    am.verified_at = Set(Some(now));
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    employee_document::Entity::find_by_id(document_id)
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("resolved employee_document missing".into()))
}

pub async fn map_file_storage_rows(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    file_ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, file_storage::Model>> {
    if file_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = file_storage::Entity::find()
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .filter(file_storage::Column::Id.is_in(file_ids.to_vec()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.into_iter().map(|m| (m.id, m)).collect())
}

pub async fn map_document_type_rows(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    type_ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, document_type::Model>> {
    if type_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = document_type::Entity::find()
        .filter(document_type::Column::TenantId.eq(tenant_id))
        .filter(document_type::Column::Id.is_in(type_ids.to_vec()))
        .filter(document_type::Column::IsDeleted.eq(false))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.into_iter().map(|m| (m.id, m)).collect())
}
