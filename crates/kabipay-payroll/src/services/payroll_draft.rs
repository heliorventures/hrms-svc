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
pub async fn find<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
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
        super::payroll_payment_date::refresh(db, &cycle, draft).await?;
    }
    Ok(draft)
}
pub async fn calculate_payroll_cycle(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &kabipay_common::context::ClientClaims,
    id: Uuid,
    expected_revision: Option<i32>,
) -> KabiPayResult<PayrollDraft> {
    super::loan_recovery::actor(claims, tenant)?;
    let actor = claims.sub;
    let txn = db.begin().await?;
    super::loan_recovery::lock(&txn, tenant).await?;
    super::payroll_fingerprint::lock_inputs(&txn).await?;
    let cycle = cycle(&txn, tenant, id).await?;
    ensure_draft(&cycle.status)?;
    let prior = find(&txn, tenant, id).await?;
    let revision = next_revision(prior.map(|d| d.revision), expected_revision)?;
    let mut employees = super::payroll_preview::employees(&txn, tenant, actor, &cycle).await?;
    for entry in &mut employees {
        if let Some(prepared) = &mut entry.prepared {
            if let Err(error) = super::loan_recovery::prepare(
                &txn,
                claims,
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
        fingerprint: super::loan_recovery::fingerprint(&txn, tenant).await?,
        employees,
        can_finalize: false,
        finalization_block_reason: None,
    };
    super::payroll_payment_date::refresh(&txn, &cycle, &mut draft).await?;
    let value = serde_json::to_value(&draft)
        .map_err(|_| KabiPayError::Internal("payroll review serialization failed".into()))?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO payroll_draft_calculation(tenant_id,cycle_id,revision,fingerprint,snapshot,calculated_by) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,cycle_id) DO UPDATE SET revision=excluded.revision,fingerprint=excluded.fingerprint,snapshot=excluded.snapshot,calculated_by=excluded.calculated_by,calculated_at=NOW()",
        [tenant.into(),id.into(),revision.into(),draft.fingerprint.clone().into(),value.into(),actor.into()])).await?;
    txn.commit().await?;
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
