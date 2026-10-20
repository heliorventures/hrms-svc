//! Employee separation / offboarding (domain 0017).

use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

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
    let tx=db.begin().await?;
    let result=insert_separation_in_transaction(&tx,tenant_id,Uuid::new_v4(),employee_id,separation_type,resignation_date,last_working_date,reason).await?;
    tx.commit().await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_separation_in_transaction(
    tx:&sea_orm::DatabaseTransaction,tenant_id:Uuid,id:Uuid,employee_id:Uuid,
    separation_type:String,resignation_date:Option<NaiveDate>,last_working_date:NaiveDate,reason:Option<String>,
)->KabiPayResult<separation::Model>{
    let t = separation_type.trim();
    if t.is_empty() {
        return Err(KabiPayError::Validation(
            "separationType is required".into(),
        ));
    }
    kabipay_common::financial_lock::acquire(tx, tenant_id).await?;
    use kabipay_db_entities::tenant::d0007_employee_core::employee;
    let employee=employee::Entity::find_by_id(employee_id).filter(employee::Column::TenantId.eq(tenant_id)).filter(employee::Column::IsDeleted.eq(false)).lock_shared().one(tx).await?.ok_or_else(||KabiPayError::NotFound{entity:"employee",id:employee_id.to_string()})?;
    if employee.status=="TERMINATED" || last_working_date<employee.date_of_joining || resignation_date.is_some_and(|date|date>last_working_date){return Err(KabiPayError::Validation("Separation dates or employment status are invalid".into()));}
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
    // Serialize submission with the funding status check and financial posting.
    am.insert(tx).await?;
    separation::Entity::find_by_id(id)
        .one(tx)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted separation not found".into()))
}

/// HR rejection uses its original principal. It cannot authorize a Finance approval.
pub(crate) async fn reject_with_principal_in_transaction(tx:&sea_orm::DatabaseTransaction,tenant:Uuid,id:Uuid,principal:Uuid)->KabiPayResult<()> {
    use sea_orm::ConnectionTrait;
    use kabipay_organization_tenant::statement;
    let changed=tx.execute(statement("UPDATE separation SET status='REJECTED',approved_by=NULL,approved_by_principal_id=$3,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='PENDING'",vec![tenant.into(),id.into(),principal.into()])).await?;
    if changed.rows_affected()!=1{return Err(KabiPayError::Conflict("Separation is no longer pending".into()));}
    crate::services::finance_operation::cancel_registered_for_separation(tx,tenant,id).await
}

pub(crate) async fn cancel_approval_in_transaction(tx:&sea_orm::DatabaseTransaction,tenant:Uuid,id:Uuid)->KabiPayResult<()> {
    use sea_orm::ConnectionTrait;
    use kabipay_organization_tenant::{statement,unavailable};
    let row=tx.query_one(statement("SELECT status FROM separation WHERE tenant_id=$1 AND id=$2 FOR UPDATE",vec![tenant.into(),id.into()])).await?.ok_or_else(unavailable)?;
    if row.try_get::<String>("","status")?!="PENDING" || tx.query_one(statement("SELECT id FROM employee_exit_operation WHERE tenant_id=$1 AND separation_id=$2 AND state='COMMITTED' LIMIT 1",vec![tenant.into(),id.into()])).await?.is_some(){return Err(KabiPayError::Conflict("Committed or resolved exit approval cannot be cancelled".into()));}
    crate::services::finance_operation::cancel_registered_for_separation(tx,tenant,id).await?;
    tx.execute(statement("UPDATE separation SET updated_at=now() WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),id.into()])).await?;
    Ok(())
}

/// HR resolves a `PENDING` row to **`APPROVED`** or **`REJECTED`**. `approver_user_id` is the acting **user** id from JWT `sub`.
pub async fn resolve_separation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    separation_id: Uuid,
    approved: bool,
    approver_user_id: Uuid,
    _business_date: NaiveDate,
) -> KabiPayResult<separation::Model> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    if approved {
        return Err(KabiPayError::Forbidden(
            "Separation approval requires a verified Finance authorization".into(),
        ));
    }
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
    let new_status = "REJECTED";
    let now = chrono::Utc::now();
    let mut am: separation::ActiveModel = row.into();
    am.status = Set(new_status.to_string());
    am.approved_by = Set(Some(approver_user_id));
    am.updated_at = Set(now);
    am.update(&txn).await?;
    crate::services::finance_operation::cancel_registered_for_separation(
        &txn,
        tenant_id,
        separation_id,
    )
    .await?;
    txn.commit().await?;
    separation::Entity::find_by_id(separation_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("separation not found after update".into()))
}
