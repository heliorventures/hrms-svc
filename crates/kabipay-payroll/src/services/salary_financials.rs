//! Optional imported annual values; an unknown employer cost is never presented as a final CTC.
use kabipay_common::KabiPayResult;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;
pub async fn for_assignment<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Option<Uuid>,
) -> KabiPayResult<Option<serde_json::Value>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT annual_gross,annual_employer_pf FROM employee_salary_structure WHERE tenant_id=$1 AND id=$2",[tenant.into(),id.into()])).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let annual: Option<Decimal> = row.try_get("", "annual_gross")?;
    let Some(annual) = annual else {
        return Ok(None);
    };
    let employer: Option<Decimal> = row.try_get("", "annual_employer_pf")?;
    Ok(Some(
        serde_json::json!({"annual_gross":annual.to_string(),"annual_employer_pf":employer.map(|v|v.to_string()),"annual_ctc":employer.map(|pf|(annual+pf).to_string()),"ctc_ready":employer.is_some()}),
    ))
}
