//! Full & final (FNF) settlement and department clearance (domain 0017).

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::{clearance_checklist, fnf_settlement, separation};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use std::str::FromStr;
use uuid::Uuid;

const DEFAULT_CLEARANCE: &[(&str, &str)] = &[
    ("IT", "Return hardware; revoke system access"),
    ("HR", "Exit interview; collect ID card"),
    ("Finance", "Final dues, recoverables, and FNF review"),
    ("Admin", "Keys, access card, and workplace clearance"),
];

fn dec_opt(s: &Option<String>) -> KabiPayResult<Option<Decimal>> {
    match s {
        None => Ok(None),
        Some(t) if t.trim().is_empty() => Ok(None),
        Some(t) => Decimal::from_str(t.trim())
            .map_err(|e| KabiPayError::Validation(format!("invalid amount: {e}")))
            .map(Some),
    }
}

fn or_zero(d: &Option<Decimal>) -> Decimal {
    d.as_ref().copied().unwrap_or(Decimal::ZERO)
}

/// After HR approves a separation, create an empty FNF (DRAFT) row and default clearance lines if missing.
/// Call within the same DB transaction as the separation status update.
pub async fn ensure_artifacts_on_approval<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    separation_id: Uuid,
) -> KabiPayResult<()> {
    let existing_fnf = fnf_settlement::Entity::find()
        .filter(fnf_settlement::Column::TenantId.eq(tenant_id))
        .filter(fnf_settlement::Column::SeparationId.eq(separation_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    if existing_fnf.is_none() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let am = fnf_settlement::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            separation_id: Set(separation_id),
            leave_encashment: Set(None),
            gratuity_amount: Set(None),
            bonus_payable: Set(None),
            recovery_amount: Set(None),
            net_payable: Set(None),
            status: Set("DRAFT".into()),
            processed_at: Set(None),
            processed_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.map_err(KabiPayError::from)?;
    }
    use sea_orm::PaginatorTrait;
    let count = clearance_checklist::Entity::find()
        .filter(clearance_checklist::Column::TenantId.eq(tenant_id))
        .filter(clearance_checklist::Column::SeparationId.eq(separation_id))
        .count(db)
        .await
        .map_err(KabiPayError::from)?;
    if count == 0 {
        let now = Utc::now();
        for (dept, task) in DEFAULT_CLEARANCE {
            let am = clearance_checklist::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                separation_id: Set(separation_id),
                department: Set((*dept).to_string()),
                task_name: Set((*task).to_string()),
                is_cleared: Set(false),
                cleared_by: Set(None),
                cleared_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db).await.map_err(KabiPayError::from)?;
        }
    }
    Ok(())
}

pub async fn get_fnf_by_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
) -> KabiPayResult<Option<fnf_settlement::Model>> {
    fnf_settlement::Entity::find()
        .filter(fnf_settlement::Column::TenantId.eq(tenant_id))
        .filter(fnf_settlement::Column::SeparationId.eq(separation_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
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

pub async fn get_visible_fnf_by_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    employee_id: Option<Uuid>,
) -> KabiPayResult<Option<fnf_settlement::Model>> {
    if get_visible_separation(db, tenant_id, separation_id, employee_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    get_fnf_by_separation(db, tenant_id, separation_id).await
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
    ensure_artifacts_on_approval(&txn, tenant_id, separation_id).await?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(())
}

/// HR fills amounts while status is DRAFT.
pub async fn upsert_fnf_settlement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    leave_encashment: &Option<String>,
    gratuity_amount: &Option<String>,
    bonus_payable: &Option<String>,
    recovery_amount: &Option<String>,
) -> KabiPayResult<fnf_settlement::Model> {
    let row = get_fnf_by_separation(db, tenant_id, separation_id)
        .await?
        .ok_or_else(|| {
            KabiPayError::NotFound {
                entity: "fnf_settlement",
                id: separation_id.to_string(),
            }
        })?;
    if row.status != "DRAFT" {
        return Err(KabiPayError::Validation(
            "FNF is not in DRAFT; cannot edit amounts".into(),
        ));
    }
    let fid = row.id;
    let leave = dec_opt(leave_encashment)?;
    let grat = dec_opt(gratuity_amount)?;
    let bonus = dec_opt(bonus_payable)?;
    let rec = dec_opt(recovery_amount)?;
    let net = {
        let sum = or_zero(&leave) + or_zero(&grat) + or_zero(&bonus);
        let r = or_zero(&rec);
        if sum >= r {
            Some(sum - r)
        } else {
            Some(Decimal::ZERO)
        }
    };
    let now = Utc::now();
    let mut am: fnf_settlement::ActiveModel = row.into();
    am.leave_encashment = Set(leave);
    am.gratuity_amount = Set(grat);
    am.bonus_payable = Set(bonus);
    am.recovery_amount = Set(rec);
    am.net_payable = Set(net);
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    fnf_settlement::Entity::find_by_id(fid)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("fnf_settlement not found after update".into()))
}

pub async fn finalize_fnf_settlement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    processed_by: Uuid,
) -> KabiPayResult<fnf_settlement::Model> {
    let row = get_fnf_by_separation(db, tenant_id, separation_id)
        .await?
        .ok_or_else(|| {
            KabiPayError::NotFound {
                entity: "fnf_settlement",
                id: separation_id.to_string(),
            }
        })?;
    if row.status != "DRAFT" {
        return Err(KabiPayError::Validation("FNF is not in DRAFT".into()));
    }
    let fid = row.id;
    let now = Utc::now();
    let mut am: fnf_settlement::ActiveModel = row.into();
    am.status = Set("PROCESSED".into());
    am.processed_at = Set(Some(now));
    am.processed_by = Set(Some(processed_by));
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    fnf_settlement::Entity::find_by_id(fid)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("fnf_settlement not found after finalize".into()))
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
