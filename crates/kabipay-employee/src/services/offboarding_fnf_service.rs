//! Full & final (FNF) settlement and department clearance (domain 0017).

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::{clearance_checklist, separation};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set,
};
use uuid::Uuid;

const DEFAULT_CLEARANCE: &[(&str, &str)] = &[
    ("IT", "Return hardware; revoke system access"),
    ("HR", "Exit interview; collect ID card"),
    ("Finance", "Final dues, recoverables, and FNF review"),
    ("Admin", "Keys, access card, and workplace clearance"),
];

/// Employee owns operational clearance only; Finance owns the FNF draft.
pub async fn ensure_clearance_on_approval<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    separation_id: Uuid,
) -> KabiPayResult<()> {
    use sea_orm::PaginatorTrait;
    let count = clearance_checklist::Entity::find()
        .filter(clearance_checklist::Column::TenantId.eq(tenant_id))
        .filter(clearance_checklist::Column::SeparationId.eq(separation_id))
        .count(db)
        .await?;
    if count != 0 {
        return Ok(());
    }
    let now = Utc::now();
    for (department, task) in DEFAULT_CLEARANCE {
        clearance_checklist::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            separation_id: Set(separation_id),
            department: Set((*department).into()),
            task_name: Set((*task).into()),
            is_cleared: Set(false),
            cleared_by: Set(None),
            cleared_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

pub async fn list_clearance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
) -> KabiPayResult<Vec<clearance_checklist::Model>> {
    clearance_checklist::Entity::find()
        .filter(clearance_checklist::Column::TenantId.eq(tenant_id))
        .filter(clearance_checklist::Column::SeparationId.eq(separation_id))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn get_separation_tenant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
) -> KabiPayResult<Option<separation::Model>> {
    separation::Entity::find_by_id(separation_id)
        .filter(separation::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

/// Load a separation only when it matches the resolver-supplied employee predicate.
/// `None` means tenant-wide authority; `Some` is JWT-bound self-service.
pub async fn get_visible_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    employee_id: Option<Uuid>,
) -> KabiPayResult<Option<separation::Model>> {
    let mut query = separation::Entity::find_by_id(separation_id)
        .filter(separation::Column::TenantId.eq(tenant_id));
    if let Some(employee_id) = employee_id {
        query = query.filter(separation::Column::EmployeeId.eq(employee_id));
    }
    query.one(db).await.map_err(KabiPayError::from)
}

pub async fn list_visible_clearance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    employee_id: Option<Uuid>,
) -> KabiPayResult<Vec<clearance_checklist::Model>> {
    if get_visible_separation(db, tenant_id, separation_id, employee_id)
        .await?
        .is_none()
    {
        return Ok(Vec::new());
    }
    list_clearance(db, tenant_id, separation_id).await
}

/// For legacy `APPROVED` separations (before auto-seed): HR may create DRAFT FNF + default clearance once.
pub async fn backfill_approved_artifacts(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    claims: &kabipay_common::context::ClientClaims,
) -> KabiPayResult<()> {
    let sep = get_separation_tenant(db, tenant_id, separation_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "separation",
            id: separation_id.to_string(),
        })?;
    if sep.status != "APPROVED" {
        return Err(KabiPayError::Validation(
            "separation must be APPROVED to backfill FNF and clearance".into(),
        ));
    }
    use sea_orm::TransactionTrait;
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    ensure_clearance_on_approval(&txn, tenant_id, separation_id).await?;
    txn.commit().await.map_err(KabiPayError::from)?;
    // Finance creates its own draft after the HR transaction has committed.
    kabipay_common::finance_transport::send(
        kabipay_common::finance_contract::ServiceCaller::Employee,
        kabipay_common::finance_contract::ServiceCaller::Payroll,
        tenant_id,
        Some(claims),
        kabipay_common::finance_contract::FinanceAction::EnsureFnf,
        serde_json::json!({"separationId": separation_id}),
    )
    .await?;
    Ok(())
}

pub async fn set_clearance_cleared(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    clearance_id: Uuid,
    cleared: bool,
    user_id: Uuid,
) -> KabiPayResult<clearance_checklist::Model> {
    let row = clearance_checklist::Entity::find_by_id(clearance_id)
        .filter(clearance_checklist::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "clearance_checklist",
            id: clearance_id.to_string(),
        })?;
    let cid = row.id;
    let now = Utc::now();
    let mut am: clearance_checklist::ActiveModel = row.into();
    am.is_cleared = Set(cleared);
    if cleared {
        am.cleared_by = Set(Some(user_id));
        am.cleared_at = Set(Some(now));
    } else {
        am.cleared_by = Set(None);
        am.cleared_at = Set(None);
    }
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    clearance_checklist::Entity::find_by_id(cid)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("clearance not found after update".into()))
}
