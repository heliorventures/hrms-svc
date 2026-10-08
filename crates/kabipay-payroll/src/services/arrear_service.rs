use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0035_payroll_arrear::payroll_arrear;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use uuid::Uuid;

pub const STATUS_PENDING: &str = "PENDING";
pub const STATUS_APPLIED: &str = "APPLIED";

use chrono::Utc;
use sea_orm::prelude::DateTimeUtc;

/// Per-employee `PENDING` arrear `id` + `amount` (to attach lines and mark applied in one txn).
pub async fn list_pending_by_employee(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Vec<payroll_arrear::Model>> {
    let rows = payroll_arrear::Entity::find()
        .filter(payroll_arrear::Column::TenantId.eq(tenant_id))
        .filter(payroll_arrear::Column::EmployeeId.eq(employee_id))
        .filter(payroll_arrear::Column::Status.eq(STATUS_PENDING))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows)
}

/// Insert a PENDING arrear to be included on the next pay run (same RBAC as pay run in GraphQL).
pub async fn create_arrear(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    employee_id: Uuid,
    amount: rust_decimal::Decimal,
    reason: Option<String>,
) -> KabiPayResult<payroll_arrear::Model> {
    kabipay_tax::domain::validate_amount(amount)?;
    if amount <= rust_decimal::Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "arrear amount must be positive".into(),
        ));
    }
    let id = Uuid::new_v4();
    let now: DateTimeUtc = Utc::now();
    let am = payroll_arrear::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        amount: Set(amount),
        reason: Set(reason),
        status: Set(STATUS_PENDING.to_string()),
        applied_payroll_cycle_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)
}

/// Mark all given arrear rows as `APPLIED` for a processed cycle.
pub async fn mark_applied(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    arrear_ids: &[Uuid],
    cycle_id: Uuid,
) -> KabiPayResult<()> {
    if arrear_ids.is_empty() {
        return Ok(());
    }
    for aid in arrear_ids {
        let m = payroll_arrear::Entity::find()
            .filter(payroll_arrear::Column::Id.eq(*aid))
            .filter(payroll_arrear::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "payroll_arrear",
                id: aid.to_string(),
            })?;
        if m.status != STATUS_PENDING {
            return Err(KabiPayError::Validation(
                "Arrear has already been applied; recalculate the draft".into(),
            ));
        }
        let mut a: payroll_arrear::ActiveModel = m.into();
        a.status = Set(STATUS_APPLIED.to_string());
        a.applied_payroll_cycle_id = Set(Some(cycle_id));
        a.updated_at = Set(Utc::now());
        a.update(db).await.map_err(KabiPayError::from)?;
    }
    Ok(())
}

/// PENDING arrear accruals for a tenant (HR / payroll list).
pub async fn list_pending_tenant(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<payroll_arrear::Model>> {
    let limit = limit.clamp(1, 200);
    let rows = payroll_arrear::Entity::find()
        .filter(payroll_arrear::Column::TenantId.eq(tenant_id))
        .filter(payroll_arrear::Column::Status.eq(STATUS_PENDING))
        .order_by_desc(payroll_arrear::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows)
}
