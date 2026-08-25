//! Employee separation / offboarding (domain 0017).

use chrono::NaiveDate;
use kabipay_common::due_offboarding::offboard_approved_separation_in_transaction;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

use crate::services::offboarding_fnf_service;

pub async fn list_for_tenant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    employee_id_filter: Option<Uuid>,
) -> KabiPayResult<Vec<separation::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = separation::Entity::find().filter(separation::Column::TenantId.eq(tenant_id));
    if let Some(eid) = employee_id_filter {
        q = q.filter(separation::Column::EmployeeId.eq(eid));
    }
    q.order_by_desc(separation::Column::LastWorkingDate)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn insert_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    separation_type: String,
    resignation_date: Option<NaiveDate>,
    last_working_date: NaiveDate,
    reason: Option<String>,
) -> KabiPayResult<separation::Model> {
    let t = separation_type.trim();
    if t.is_empty() {
        return Err(KabiPayError::Validation("separationType is required".into()));
    }
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let reason = reason.and_then(|s| {
        let x = s.trim();
        if x.is_empty() {
            None
        } else {
            Some(x.to_string())
        }
    });
    let am = separation::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        separation_type: Set(t.to_uppercase()),
        resignation_date: Set(resignation_date),
        last_working_date: Set(last_working_date),
        reason: Set(reason),
        status: Set("PENDING".into()),
        approved_by: Set(None),
        workflow_instance_id: Set(None),
        offboarded_at: Set(None),
        offboarding_event_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    separation::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted separation not found".into()))
}

/// HR resolves a `PENDING` row to **`APPROVED`** or **`REJECTED`**. `approver_user_id` is the acting **user** id from JWT `sub`.
pub async fn resolve_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    approved: bool,
    approver_user_id: Uuid,
    business_date: NaiveDate,
) -> KabiPayResult<separation::Model> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let row = separation::Entity::find_by_id(separation_id)
        .filter(separation::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "separation",
            id: separation_id.to_string(),
        })?;
    if row.status != "PENDING" {
        return Err(KabiPayError::Validation(format!(
            "separation is not pending (status={})",
            row.status
        )));
    }
    let new_status = if approved { "APPROVED" } else { "REJECTED" };
    let now = chrono::Utc::now();
    let mut am: separation::ActiveModel = row.into();
    am.status = Set(new_status.to_string());
    am.approved_by = Set(Some(approver_user_id));
    am.updated_at = Set(now);
    am.update(&txn).await?;
    if approved {
        offboarding_fnf_service::ensure_artifacts_on_approval(&txn, tenant_id, separation_id).await?;
        offboard_approved_separation_in_transaction(
            &txn,
            tenant_id,
            separation_id,
            business_date,
            now,
        )
        .await?;
    }
    txn.commit().await?;
    separation::Entity::find_by_id(separation_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("separation not found after update".into()))
}
