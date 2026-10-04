//! Imported nullable profile fields share the existing employee access boundary.
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

pub async fn read<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
) -> KabiPayResult<serde_json::Value> {
    let employee=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,confirmation_date,imported_exit_date,imported_last_working_date FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted",
        [tenant.into(),employee.into()])).await?.ok_or_else(||KabiPayError::NotFound {entity:"employee",id:employee.to_string()})?;
    let id: Uuid = employee.try_get("", "id")?;
    let banks=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT account_holder,branch_name FROM employee_bank WHERE tenant_id=$1 AND employee_id=$2 AND is_primary",
        [tenant.into(),id.into()])).await?;
    if banks.len() > 1 {
        return Err(KabiPayError::Validation(
            "primary bank requires review".into(),
        ));
    }
    let field = |name: &str| -> KabiPayResult<Option<String>> {
        banks
            .first()
            .map(|bank| bank.try_get::<Option<String>>("", name))
            .transpose()
            .map(Option::flatten)
            .map_err(Into::into)
    };
    Ok(serde_json::json!({
        "confirmation_date":employee.try_get::<Option<chrono::NaiveDate>>("","confirmation_date")?,
        "source_exit_date":employee.try_get::<Option<chrono::NaiveDate>>("","imported_exit_date")?,
        "source_last_working_date":employee.try_get::<Option<chrono::NaiveDate>>("","imported_last_working_date")?,
        "account_holder":field("account_holder")?,"bank_branch":field("branch_name")?
    }))
}
