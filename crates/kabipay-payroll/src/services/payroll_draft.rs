//! Review snapshots never create payslips or consume leave/arrears.
use super::automatic_payroll::PreparedEmployeePayroll;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::payroll_cycle;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DraftEmployee {
    pub employee_id: Uuid,
    #[serde(default)]
    pub employee_label: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub prepared: Option<PreparedEmployeePayroll>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayrollDraft {
    pub cycle_id: Uuid,
    pub revision: i32,
    pub fingerprint: String,
    pub employees: Vec<DraftEmployee>,
    pub can_finalize: bool,
    #[serde(default)]
    pub finalization_block_reason: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeAcknowledgement {
    pub provisional_tax_employees: Vec<Uuid>,
}

pub fn ensure_draft(status: &str) -> KabiPayResult<()> {
    if status != "DRAFT" {
        return Err(KabiPayError::Validation(
            "finalized or locked payroll cannot be recalculated or edited".into(),
        ));
    }
    Ok(())
}
pub fn next_revision(current: Option<i32>, expected: Option<i32>) -> KabiPayResult<i32> {
    if current != expected {
        return Err(KabiPayError::Validation(
            "payroll draft changed; reload before calculating".into(),
        ));
    }
    current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| KabiPayError::Validation("payroll revision limit reached".into()))
}
pub fn validate_acknowledgement(
    required: &[Uuid],
    value: &FinalizeAcknowledgement,
) -> KabiPayResult<()> {
    let mut expected = required.to_vec();
    expected.sort_unstable();
    let mut supplied = value.provisional_tax_employees.clone();
    supplied.sort_unstable();
    if expected != supplied {
        return Err(KabiPayError::Validation(
            "acknowledge each employee with provisional tax in this reviewed draft".into(),
        ));
    }
    Ok(())
}
pub async fn cycle<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
) -> KabiPayResult<payroll_cycle::Model> {
    payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant))
        .filter(payroll_cycle::Column::Id.eq(id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "payroll_cycle",
            id: id.to_string(),
        })
}
pub async fn find<C:ConnectionTrait>(db:&C,tenant:Uuid,id:Uuid)->KabiPayResult<Option<PayrollDraft>> {find_with_date(db,tenant,id,None).await}

pub(crate) async fn find_with_date<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    current_date:Option<chrono::NaiveDate>,
) -> KabiPayResult<Option<PayrollDraft>> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT snapshot FROM payroll_draft_calculation WHERE tenant_id=$1 AND cycle_id=$2",
            [tenant.into(), id.into()],
        ))
        .await?;
    let mut draft = row
        .map(|r| {
            serde_json::from_value(r.try_get::<serde_json::Value>("", "snapshot")?)
                .map_err(|_| KabiPayError::Internal("stored payroll review is invalid".into()))
        })
        .transpose()?;
    if let Some(draft) = &mut draft {
        let cycle = cycle(db, tenant, id).await?;
        if current_date.is_some(){super::payroll_payment_date::refresh_with_date(db, &cycle, draft,current_date).await?;}else{super::payroll_payment_date::refresh(db,&cycle,draft).await?;}
    }
    Ok(draft)
}
pub async fn calculate_payroll_cycle(db:&DatabaseConnection,tenant:Uuid,claims:&kabipay_common::context::ClientClaims,id:Uuid,expected_revision:Option<i32>)->KabiPayResult<PayrollDraft> {
 let actor=super::payroll_actor::PayrollActor::local(claims,tenant)?;
 let txn=db.begin().await?;
 let draft=calculate_in_transaction(&txn,tenant,&actor,id,expected_revision).await?;
 txn.commit().await?;Ok(draft)
}

pub async fn calculate_in_transaction(
    txn: &sea_orm::DatabaseTransaction,
    tenant: Uuid,
    actor: &super::payroll_actor::PayrollActor,
    id: Uuid,
    expected_revision: Option<i32>,
) -> KabiPayResult<PayrollDraft> {
    if actor.tenant!=tenant {return Err(KabiPayError::Unauthorised);}
    super::loan_recovery::lock(txn, tenant).await?;
    super::payroll_fingerprint::lock_inputs(txn).await?;
    let cycle = cycle(txn, tenant, id).await?;
    ensure_draft(&cycle.status)?;
    let prior = find_with_date(txn, tenant, id,actor.business_date).await?;
    let revision = next_revision(prior.map(|d| d.revision), expected_revision)?;
    let mut employees = super::payroll_preview::employees_with_actor(txn, tenant, actor.local_user, actor.principal, &cycle).await?;
    for entry in &mut employees {
        if let Some(prepared) = &mut entry.prepared {
            if let Err(error) = super::loan_recovery::prepare_with_actor(
                txn,
                &actor.loans,
                &cycle,
                revision,
                entry.employee_id,
                prepared,
            )
            .await
            {
                entry.outcome = "REVIEW".into();
                entry.reason = Some(super::payroll_preview::review_reason(error));
                entry.prepared = None;
            }
        }
    }
    let mut draft = PayrollDraft {
        cycle_id: id,
        revision,
        fingerprint: super::loan_recovery::fingerprint_with_actor(txn, tenant, &actor.loans).await?,
        employees,
        can_finalize: false,
        finalization_block_reason: None,
    };
    super::payroll_payment_date::refresh_with_date(txn, &cycle, &mut draft,actor.business_date).await?;
    let value = serde_json::to_value(&draft)
        .map_err(|_| KabiPayError::Internal("payroll review serialization failed".into()))?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO payroll_draft_calculation(tenant_id,cycle_id,revision,fingerprint,snapshot,calculated_by,calculated_by_principal_id) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant_id,cycle_id) DO UPDATE SET revision=excluded.revision,fingerprint=excluded.fingerprint,snapshot=excluded.snapshot,calculated_by=excluded.calculated_by,calculated_by_principal_id=excluded.calculated_by_principal_id,calculated_at=NOW()",
        [tenant.into(),id.into(),revision.into(),draft.fingerprint.clone().into(),value.into(),actor.local_user.into(),actor.principal.into()])).await?;
    Ok(draft)
}

/// Correct a draft's explicit recovery date; every existing quote becomes stale.
pub async fn set_payment_date(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &kabipay_common::context::ClientClaims,
    id: Uuid,
    date: chrono::NaiveDate,
    expected_revision: Option<i32>,
) -> KabiPayResult<()> {
    super::loan_recovery::actor(claims, tenant)?;
    let tx = db.begin().await?;
    super::loan_recovery::lock(&tx, tenant).await?;
    super::payroll_fingerprint::lock_inputs(&tx).await?;
    let cycle = cycle(&tx, tenant, id).await?;
    ensure_draft(&cycle.status)?;
    let prior = find(&tx, tenant, id).await?.ok_or_else(|| {
        KabiPayError::Validation("Calculate a draft before changing its payment date".into())
    })?;
    if Some(prior.revision) != expected_revision
        || super::loan_recovery::fingerprint(&tx, tenant).await? != prior.fingerprint
    {
        return Err(KabiPayError::Conflict(
            "Payroll review changed; reload before editing its payment date".into(),
        ));
    }
    let row = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM payslip WHERE tenant_id=$1 AND payroll_cycle_id=$2",
            [tenant.into(), id.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("Payslip verification unavailable".into()))?;
    if row.try_get::<i64>("", "n")? != 0 {
        return Err(KabiPayError::Validation(
            "A cycle with issued financial records cannot be edited".into(),
        ));
    }
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE payroll_cycle SET payment_date=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2",
        [tenant.into(), id.into(), date.into()],
    ))
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
#[path = "payroll_draft_date_tests.rs"]
mod date_tests;
