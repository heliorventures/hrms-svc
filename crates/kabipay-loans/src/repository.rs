//! The only storage boundary for Loans. All calls use the caller's transaction.
use crate::{LoanModuleError, LoanResult};
use kabipay_db_entities::tenant::d0103_employee_loans::{
    loan_account, loan_policy_version, loan_request, loan_terms_version,
};
use rust_decimal::Decimal;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, QueryFilter,
    QueryResult, QuerySelect, Statement, Value,
};
use uuid::Uuid;

pub(crate) async fn execute(
    tx: &DatabaseTransaction,
    sql: &str,
    values: Vec<Value>,
) -> LoanResult<u64> {
    Ok(tx
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .rows_affected())
}
pub(crate) async fn one(
    tx: &DatabaseTransaction,
    sql: &str,
    values: Vec<Value>,
) -> LoanResult<Option<QueryResult>> {
    Ok(tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?)
}
pub(crate) async fn all(
    tx: &DatabaseTransaction,
    sql: &str,
    values: Vec<Value>,
) -> LoanResult<Vec<QueryResult>> {
    Ok(tx
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?)
}

/// Obtain before any employee/account/payroll locks. Shared by every financial writer.
pub(crate) async fn synchronize(tx: &DatabaseTransaction, tenant: Uuid) -> LoanResult<()> {
    one(
        tx,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("financial:{tenant}").into()],
    )
    .await?;
    Ok(())
}
pub(crate) async fn request(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> LoanResult<loan_request::Model> {
    loan_request::Entity::find_by_id(id)
        .filter(loan_request::Column::TenantId.eq(tenant))
        .lock_exclusive()
        .one(tx)
        .await?
        .ok_or(LoanModuleError::NotFound)
}
pub(crate) async fn account(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> LoanResult<loan_account::Model> {
    loan_account::Entity::find_by_id(id)
        .filter(loan_account::Column::TenantId.eq(tenant))
        .lock_exclusive()
        .one(tx)
        .await?
        .ok_or(LoanModuleError::NotFound)
}
pub(crate) async fn terms(
    tx: &DatabaseTransaction,
    account: &loan_account::Model,
) -> LoanResult<loan_terms_version::Model> {
    loan_terms_version::Entity::find_by_id(
        account
            .current_terms_id
            .ok_or(LoanModuleError::InvalidCommand)?,
    )
    .filter(loan_terms_version::Column::TenantId.eq(account.tenant_id))
    .filter(loan_terms_version::Column::LoanId.eq(account.id))
    .one(tx)
    .await?
    .ok_or(LoanModuleError::NotFound)
}
pub(crate) async fn policy(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> LoanResult<loan_policy_version::Model> {
    loan_policy_version::Entity::find_by_id(id)
        .filter(loan_policy_version::Column::TenantId.eq(tenant))
        .one(tx)
        .await?
        .ok_or(LoanModuleError::PolicyUnavailable)
}
pub(crate) async fn balances(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
) -> LoanResult<(Decimal, Decimal)> {
    let row = one(tx, "SELECT COALESCE(SUM(principal_delta),0)::text AS principal, COALESCE(SUM(interest_delta),0)::text AS interest FROM loan_ledger_entry WHERE tenant_id=$1 AND loan_id=$2", vec![tenant.into(), loan.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    Ok((decimal(&row, "principal")?, decimal(&row, "interest")?))
}
pub(crate) fn decimal(row: &QueryResult, column: &str) -> LoanResult<Decimal> {
    let value: String = row.try_get("", column)?;
    value.parse().map_err(|_| LoanModuleError::InvalidCommand)
}
pub(crate) async fn revision(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
) -> LoanResult<i64> {
    execute(tx,"INSERT INTO loan_employee_state(id,tenant_id,employee_id,revision) VALUES($1,$2,$3,0) ON CONFLICT(tenant_id,employee_id) DO NOTHING",vec![Uuid::new_v4().into(),tenant.into(),employee.into()]).await?;
    let row = one(
        tx,
        "SELECT revision FROM loan_employee_state WHERE tenant_id=$1 AND employee_id=$2 FOR UPDATE",
        vec![tenant.into(), employee.into()],
    )
    .await?
    .ok_or(LoanModuleError::NotFound)?;
    Ok(row.try_get("", "revision")?)
}
pub(crate) async fn advance_revision(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
    action: &str,
) -> LoanResult<i64> {
    revision(tx, tenant, employee).await?;
    let row=one(tx,"UPDATE loan_employee_state SET revision=revision+1 WHERE tenant_id=$1 AND employee_id=$2 RETURNING revision",vec![tenant.into(),employee.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let version: i64 = row.try_get("", "revision")?;
    execute(tx,"INSERT INTO loan_internal_outbox(id,tenant_id,employee_id,event_type,aggregate_revision,payload,available_at,attempts) VALUES($1,$2,$3,$4,$5,$6,NOW(),0)",vec![Uuid::new_v4().into(),tenant.into(),employee.into(),action.into(),version.into(),serde_json::json!({"employeeId":employee,"revision":version}).into()]).await?;
    Ok(version)
}
