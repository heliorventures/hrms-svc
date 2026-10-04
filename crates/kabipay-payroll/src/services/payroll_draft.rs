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
    row.map(|r| {
        serde_json::from_value(r.try_get::<serde_json::Value>("", "snapshot")?)
            .map_err(|_| KabiPayError::Internal("stored payroll review is invalid".into()))
    })
    .transpose()
}
pub async fn calculate_payroll_cycle(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    id: Uuid,
    expected_revision: Option<i32>,
) -> KabiPayResult<PayrollDraft> {
    let txn = db.begin().await?;
    super::payroll_fingerprint::lock_inputs(&txn).await?;
    let cycle = cycle(&txn, tenant, id).await?;
    ensure_draft(&cycle.status)?;
    let prior = find(&txn, tenant, id).await?;
    let revision = next_revision(prior.map(|d| d.revision), expected_revision)?;
    let employees = super::payroll_preview::employees(&txn, tenant, &cycle).await?;
    let can_finalize = employees.iter().any(|e| e.outcome == "READY")
        && !employees.iter().any(|e| e.outcome == "REVIEW");
    let draft = PayrollDraft {
        cycle_id: id,
        revision,
        fingerprint: super::payroll_fingerprint::fingerprint(&txn, tenant).await?,
        employees,
        can_finalize,
    };
    let value = serde_json::to_value(&draft)
        .map_err(|_| KabiPayError::Internal("payroll review serialization failed".into()))?;
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO payroll_draft_calculation(tenant_id,cycle_id,revision,fingerprint,snapshot,calculated_by) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,cycle_id) DO UPDATE SET revision=excluded.revision,fingerprint=excluded.fingerprint,snapshot=excluded.snapshot,calculated_by=excluded.calculated_by,calculated_at=NOW()",
        [tenant.into(),id.into(),revision.into(),draft.fingerprint.clone().into(),value.into(),actor.into()])).await?;
    txn.commit().await?;
    Ok(draft)
}
