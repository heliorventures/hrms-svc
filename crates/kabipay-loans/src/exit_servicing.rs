//! Loan-owned immutable exit servicing decisions. No repayment is fabricated here.
use crate::{repository as store, ExitFinancialDecision, ExitLoanDecision, LoanModuleError, LoanResult};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    tx: &DatabaseTransaction, tenant: Uuid, employee: Uuid, separation: Uuid,
    origin_kind: &str, origin: Uuid, actor: Uuid, last_working: NaiveDate,
    settlement: NaiveDate, decision: &ExitFinancialDecision,
) -> LoanResult<()> {
    let mut changed = false;
    for line in &decision.loans {
        let hash = crate::command_hash(&(separation, last_working, settlement, line))?;
        let existing = store::one(tx,
            "SELECT decision_hash FROM loan_exit_arrangement WHERE tenant_id=$1 AND origin_kind=$2 AND origin_id=$3 AND loan_id=$4",
            vec![tenant.into(),origin_kind.into(),origin.into(),line.loan_id.into()]).await?;
        if let Some(existing) = existing {
            if existing.try_get::<String>("", "decision_hash")? != hash {
                return Err(LoanModuleError::IdempotencyConflict);
            }
            continue;
        }
        store::execute(tx,
            "INSERT INTO loan_exit_arrangement(id,tenant_id,loan_id,employee_id,separation_id,origin_kind,origin_id,last_working_date,settlement_date,decision,decision_hash,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            vec![Uuid::new_v4().into(),tenant.into(),line.loan_id.into(),employee.into(),separation.into(),origin_kind.into(),origin.into(),last_working.into(),settlement.into(),serde_json::to_value(line)?.into(),hash.into(),actor.into()]).await?;
        changed = true;
    }
    if changed { store::advance_revision(tx,tenant,employee,"EXIT_ARRANGEMENT_RECORDED").await?; }
    Ok(())
}

/// Called by FNF only after its reviewed source allocations were posted in the same transaction.
#[allow(clippy::too_many_arguments)]
pub async fn confirm_fnf_arrangements(
    tx: &DatabaseTransaction, actor: &crate::LoanActorScope, employee: Uuid,
    separation: Uuid, fnf: Uuid, last_working: NaiveDate, settlement: NaiveDate,
    decision: &ExitFinancialDecision,
) -> LoanResult<()> {
    if actor.permission != crate::LoanPermission::FnfRecovery { return Err(LoanModuleError::Forbidden); }
    crate::access::company(actor)?;
    crate::access::employee(tx,actor,employee).await?;
    store::synchronize(tx,actor.tenant_id).await?;
    record(tx,actor.tenant_id,employee,separation,"FNF",fnf,actor.user_id,last_working,settlement,decision).await
}

pub(crate) async fn payroll_limit(
    tx: &DatabaseTransaction, tenant: Uuid, loan: Uuid, cycle: Uuid, value_date: NaiveDate,
) -> LoanResult<Option<Decimal>> {
    let row = store::one(tx,
        "SELECT origin_kind,last_working_date,decision FROM loan_exit_arrangement WHERE tenant_id=$1 AND loan_id=$2 ORDER BY CASE WHEN origin_kind='FNF' THEN 0 ELSE 1 END,created_at DESC,id DESC LIMIT 1",
        vec![tenant.into(),loan.into()]).await?;
    let Some(row) = row else { return Ok(None); };
    if row.try_get::<String>("","origin_kind")? == "FNF" { return Ok(Some(Decimal::ZERO)); }
    let decision: ExitLoanDecision = serde_json::from_value(row.try_get("","decision")?)?;
    if decision.final_payroll_cycle_id == Some(cycle) { return Ok(Some(decision.proposed_payroll_recovery)); }
    if value_date > row.try_get::<NaiveDate>("","last_working_date")? { return Ok(Some(Decimal::ZERO)); }
    Ok(None)
}
