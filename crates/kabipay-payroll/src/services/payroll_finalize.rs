//! All financial writes and the immutable cycle transition share one transaction.
use super::payroll_draft::{self, FinalizeAcknowledgement};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayrollFinalization {
    pub cycle_id: Uuid,
    pub revision: i32,
    pub status: String,
    pub payslips: usize,
}

pub async fn finalize_payroll_cycle(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &kabipay_common::context::ClientClaims,
    id: Uuid,
    revision: i32,
    fingerprint: &str,
    acknowledgement: FinalizeAcknowledgement,
) -> KabiPayResult<PayrollFinalization> {
    super::loan_recovery::actor(claims, tenant)?;
    let actor = claims.sub;
    let txn = db.begin().await?;
    super::loan_recovery::lock(&txn, tenant).await?;
    super::payroll_fingerprint::lock_inputs(&txn).await?;
    let cycle = payroll_draft::cycle(&txn, tenant, id).await?;
    payroll_draft::ensure_draft(&cycle.status)?;
    let draft = payroll_draft::find(&txn, tenant, id)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("Calculate and review payroll before finalizing".into())
        })?;
    if draft.revision != revision
        || draft.fingerprint != fingerprint
        || super::loan_recovery::fingerprint(&txn, tenant).await? != fingerprint
    {
        return Err(KabiPayError::Validation(
            "Payroll inputs changed; recalculate and review the new draft".into(),
        ));
    }
    if let Some(reason) = &draft.finalization_block_reason {
        return Err(KabiPayError::Validation(reason.clone()));
    }
    if !draft.can_finalize {
        return Err(KabiPayError::Validation(
            "Resolve employee review items before finalizing payroll".into(),
        ));
    }
    let required: Vec<_> = draft
        .employees
        .iter()
        .filter(|e| {
            e.prepared
                .as_ref()
                .is_some_and(|p| p.requires_tax_acknowledgement)
        })
        .map(|e| e.employee_id)
        .collect();
    payroll_draft::validate_acknowledgement(&required, &acknowledgement)?;
    let date = NaiveDate::from_ymd_opt(cycle.year, cycle.month as u32, 1)
        .ok_or_else(|| KabiPayError::Validation("invalid payroll month".into()))?;
    let existing = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM payslip WHERE tenant_id=$1 AND payroll_cycle_id=$2",
            [tenant.into(), id.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("cannot verify existing payslips".into()))?;
    if existing.try_get::<i64>("", "n")? != 0 {
        return Err(KabiPayError::Validation(
            "Cycle already has financial records; finalization refused".into(),
        ));
    }
    let mut payslips = 0;
    let loan_state = kabipay_loans::payroll_loan_state(&txn, tenant)
        .await
        .map_err(|_| {
            KabiPayError::Validation("Review Loans configuration before finalizing payroll".into())
        })?;
    for entry in &draft.employees {
        let Some(prepared) = &entry.prepared else {
            continue;
        };
        super::loan_recovery::validate_calculation(
            prepared,
            &cycle,
            entry.employee_id,
            revision,
            loan_state.enabled,
        )?;
        let loan_snapshot = match &prepared.loan_recovery {
            Some(reviewed) => Some(super::loan_recovery::post(&txn, claims, reviewed).await?),
            None => None,
        };
        let employee = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant))
            .filter(employee::Column::Id.eq(entry.employee_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Validation("Reviewed employee no longer exists".into()))?;
        let payslip = super::imported_payroll::persist_reviewed(
            &txn,
            tenant,
            id,
            &employee,
            date,
            prepared.clone(),
            loan_snapshot.as_ref(),
        )
        .await?;
        if let Some(snapshot) = &loan_snapshot {
            super::loan_recovery::persist(&txn, tenant, payslip, snapshot).await?;
        }
        payslips += 1;
    }
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE payroll_cycle SET status='PROCESSED',processed_by=$3,processed_at=NOW(),updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND status='DRAFT'",
        [tenant.into(),id.into(),actor.into()])).await?;
    let ack = serde_json::to_value(&acknowledgement)
        .map_err(|_| KabiPayError::Internal("acknowledgement serialization failed".into()))?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE payroll_draft_calculation SET finalized_at=NOW(),finalized_by=$3,acknowledgement=$4 WHERE tenant_id=$1 AND cycle_id=$2",
        [tenant.into(),id.into(),actor.into(),ack.into()])).await?;
    txn.commit().await?;
    Ok(PayrollFinalization {
        cycle_id: id,
        revision,
        status: "PROCESSED".into(),
        payslips,
    })
}
