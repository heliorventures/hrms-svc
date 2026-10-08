//! Company catalog visibility. Presentation changes never modify calculated amounts.
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::HashMap;
use uuid::Uuid;

pub async fn catalog<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
) -> KabiPayResult<HashMap<Uuid, bool>> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id,show_on_payslip FROM salary_component WHERE tenant_id=$1",
            [tenant.into()],
        ))
        .await?;
    rows.into_iter()
        .map(|row| Ok((row.try_get("", "id")?, row.try_get("", "show_on_payslip")?)))
        .collect()
}
pub async fn save<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    visible: bool,
) -> KabiPayResult<()> {
    let result=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE salary_component SET show_on_payslip=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2",
        [tenant.into(),id.into(),visible.into()])).await?;
    if result.rows_affected() != 1 {
        return Err(KabiPayError::Validation(
            "salary component does not belong to this company".into(),
        ));
    }
    Ok(())
}

/// HR deductions belong to the company component catalog so the same visibility rules apply.
pub async fn ensure_additional_deduction<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    code: &str,
) -> KabiPayResult<()> {
    if !super::payroll_rules::valid_additional_code(code) {
        return Err(KabiPayError::Validation(
            "invalid or reserved additional deduction code".into(),
        ));
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT type,is_active FROM salary_component WHERE tenant_id=$1 AND code=$2",
            [tenant.into(), code.into()],
        ))
        .await?;
    if rows.len() > 1
        || rows.first().is_some_and(|row| {
            row.try_get::<String>("", "type").ok().as_deref() != Some("DEDUCTION")
                || row.try_get::<bool>("", "is_active").ok() != Some(true)
        })
    {
        return Err(KabiPayError::Validation(
            "additional deduction conflicts with the company catalog".into(),
        ));
    }
    if rows.is_empty() {
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO salary_component(id,tenant_id,name,code,type,is_taxable,is_fixed,is_active,show_on_payslip) VALUES($1,$2,$3,$4,'DEDUCTION',FALSE,FALSE,TRUE,TRUE)",
        [Uuid::new_v4().into(),tenant.into(),code.replace('_'," ").into(),code.into()])).await?;
    }
    Ok(())
}
