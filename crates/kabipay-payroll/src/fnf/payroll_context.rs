//! Payroll-owned references for HR's exit review; issued salary is never an FNF earning.
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use serde_json::Value;
use uuid::Uuid;

pub async fn read(tx: &DatabaseTransaction, tenant: Uuid, employee: Uuid) -> KabiPayResult<Value> {
    let draft = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT c.id,c.year,c.month FROM payroll_cycle c WHERE c.tenant_id=$1 AND c.status='DRAFT' AND EXISTS(SELECT 1 FROM payroll_period_input i WHERE i.tenant_id=c.tenant_id AND i.employee_id=$2 AND i.year=c.year AND i.month=c.month) ORDER BY c.year DESC,c.month DESC,c.id",
        [tenant.into(), employee.into()],
    )).await?;
    let issued = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT p.id,c.id AS cycle_id,c.year,c.month,p.net_salary::text AS amount FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND c.status='PROCESSED' ORDER BY c.year DESC,c.month DESC,p.generated_at DESC,p.id LIMIT 12",
        [tenant.into(), employee.into()],
    )).await?;
    let cycles = draft.into_iter().map(|row| Ok(serde_json::json!({
        "id":row.try_get::<Uuid>("","id")?,"year":row.try_get::<i32>("","year")?,"month":row.try_get::<i32>("","month")?,
    }))).collect::<Result<Vec<_>,sea_orm::DbErr>>()?;
    let slips = issued.into_iter().map(|row| Ok(serde_json::json!({
        "id":row.try_get::<Uuid>("","id")?,"cycleId":row.try_get::<Uuid>("","cycle_id")?,
        "year":row.try_get::<i32>("","year")?,"month":row.try_get::<i32>("","month")?,"amount":row.try_get::<String>("","amount")?,
    }))).collect::<Result<Vec<_>,sea_orm::DbErr>>()?;
    Ok(serde_json::json!({"draftCycles":cycles,"recentIssuedPayslips":slips}))
}
