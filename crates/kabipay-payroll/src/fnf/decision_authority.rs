//! Validate references owned by Payroll/Auth before saving a Loan-owned exit decision.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_loans::ExitFinancialDecision;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

pub(super) async fn verify(tx: &DatabaseTransaction, tenant: Uuid, employee: Uuid, decision: &ExitFinancialDecision) -> KabiPayResult<()> {
    for line in &decision.loans {
        if let Some(continuing) = &line.continuing {
            let owner = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT id FROM \"user\" WHERE tenant_id=$1 AND id=$2 AND is_active AND NOT is_deleted",
                [tenant.into(),continuing.follow_up_user_id.into()])).await?;
            if owner.is_none() { return Err(KabiPayError::Validation("Repayment follow-up owner must be an active company user".into())); }
        }
        if let Some(cycle) = line.final_payroll_cycle_id {
            let row = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT c.id FROM payroll_cycle c WHERE c.tenant_id=$1 AND c.id=$2 AND c.status='DRAFT' AND EXISTS(SELECT 1 FROM payroll_period_input i WHERE i.tenant_id=c.tenant_id AND i.employee_id=$3 AND i.year=c.year AND i.month=c.month)",
                [tenant.into(),cycle.into(),employee.into()])).await?;
            if row.is_none() { return Err(KabiPayError::Validation("Final payroll recovery must reference this employee's prepared draft payroll cycle; issued salary cannot be recovered again".into())); }
        }
    }
    Ok(())
}
