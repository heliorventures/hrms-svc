//! Read and update the existing employee UAN without changing other profile fields.

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{sea_query::Expr, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::entities::d0007_employee_core::employee;
use crate::services::employee_service;

pub(super) fn normalize_uan(value: &str) -> KabiPayResult<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(KabiPayError::Validation(
            "EPF / UAN number must contain exactly 12 digits".into(),
        ));
    }
    Ok(Some(value.to_owned()))
}

pub async fn read<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Option<String>> {
    let employee = employee_service::find_by_id(db, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    Ok(employee.uan_number)
}

/// An explicit empty string clears the number. Tenant and deletion checks also apply to the write.
pub async fn set<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    value: &str,
) -> KabiPayResult<Option<String>> {
    let number = normalize_uan(value)?;
    let result = employee::Entity::update_many()
        .col_expr(employee::Column::UanNumber, Expr::value(number.clone()))
        .col_expr(employee::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(employee::Column::Id.eq(employee_id))
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    if result.rows_affected == 0 {
        return Err(KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        });
    }
    Ok(number)
}
