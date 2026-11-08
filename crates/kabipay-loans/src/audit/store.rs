use super::{FinancialIssue, LoanFinancialFacts, LoanFinancialReport};
use crate::{
    access, repository as store, LoanActorScope, LoanModuleError, LoanPermission, LoanResult,
};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub async fn authorize_loan_audit(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
) -> LoanResult<()> {
    if actor.permission != LoanPermission::Manage {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    access::company(actor)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await
}

pub async fn latest_loan_audit(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
) -> LoanResult<Option<LoanFinancialReport>> {
    authorize_loan_audit(tx, actor, loan).await?;
    store::one(tx,
        "SELECT r.report FROM loan_reconciliation_status s JOIN loan_reconciliation_run r ON r.tenant_id=s.tenant_id AND r.loan_id=s.id AND r.id=s.last_run_id WHERE s.tenant_id=$1 AND s.id=$2",
        vec![actor.tenant_id.into(), loan.into()]).await?
        .map(|row| Ok(serde_json::from_value(row.try_get("","report")?)?)).transpose()
}

/// Trusted financial host has checked its Payroll and FNF sources while holding this transaction.
pub async fn persist_loan_audit(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    facts: LoanFinancialFacts,
    source_issues: Vec<FinancialIssue>,
    verified_sources: usize,
    initiated_by: Option<Uuid>,
) -> LoanResult<LoanFinancialReport> {
    store::synchronize(tx, tenant).await?;
    let account = store::account(tx, tenant, facts.loan_id).await?;
    if account.version != facts.account_version || account.employee_id != facts.employee_id {
        return Err(LoanModuleError::VersionConflict);
    }
    if verified_sources > facts.recoveries.len()
        || (verified_sources < facts.recoveries.len() && source_issues.is_empty())
        || initiated_by.is_some_and(|id| id.is_nil())
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    let mut issues = facts.issues;
    issues.extend(source_issues);
    let mut report = LoanFinancialReport {
        id: Uuid::new_v4(),
        loan_id: facts.loan_id,
        employee_id: facts.employee_id,
        account_version: facts.account_version,
        checked_at: facts.checked_at,
        issues,
        verified_source_count: verified_sources,
        fingerprint: String::new(),
    };
    let trigger = if initiated_by.is_some() {
        "HR_REVIEW"
    } else {
        "SCHEDULED"
    };
    report.fingerprint = crate::command_hash(&(tenant, trigger, initiated_by, &report))?;
    store::execute(tx,
        "INSERT INTO loan_reconciliation_run(id,tenant_id,loan_id,employee_id,account_version,checked_at,trigger_kind,initiated_by,fingerprint,report) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        vec![report.id.into(),tenant.into(),report.loan_id.into(),report.employee_id.into(),report.account_version.into(),report.checked_at.into(),trigger.into(),initiated_by.into(),report.fingerprint.clone().into(),serde_json::to_value(&report)?.into()]).await?;
    store::execute(tx,
        "INSERT INTO loan_reconciliation_status(id,tenant_id,last_run_id,blocked,checked_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO UPDATE SET last_run_id=EXCLUDED.last_run_id,blocked=EXCLUDED.blocked,checked_at=EXCLUDED.checked_at WHERE loan_reconciliation_status.tenant_id=EXCLUDED.tenant_id",
        vec![report.loan_id.into(),tenant.into(),report.id.into(),(!report.issues.is_empty()).into(),report.checked_at.into()]).await?;
    Ok(report)
}
