//! Employee-owned pending resignation lookup; Payroll never reads separation storage.
use kabipay_common::{
    finance_contract::FundingContext, tenant_business_clock::TenantBusinessClock, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

pub async fn read(
    db: &DatabaseConnection,
    tenant: Uuid,
    employee: Uuid,
    value_date: chrono::NaiveDate,
    clock: TenantBusinessClock,
) -> KabiPayResult<FundingContext> {
    // Payment inputs have date precision; the request's local calendar day is included.
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM separation WHERE tenant_id=$1 AND employee_id=$2 AND status='PENDING' AND separation_type='RESIGNATION' AND (created_at AT TIME ZONE $3)::date <= $4 ORDER BY created_at,id LIMIT 1",
        [tenant.into(), employee.into(), clock.timezone_name().into(), value_date.into()],
    )).await?;
    Ok(FundingContext {
        employee_id: employee,
        value_date,
        pending_separation_id: row.map(|row| row.try_get("", "id")).transpose()?,
    })
}
