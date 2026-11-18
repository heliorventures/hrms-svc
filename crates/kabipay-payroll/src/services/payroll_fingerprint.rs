//! The bounded database authority owns input locks and the exact v2 digest.
//! Runtime Payroll receives neither broad input reads nor input-table writes.
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

#[cfg(test)]
#[path = "payroll_fingerprint_tests.rs"]
mod tests;

pub async fn lock_inputs(db: &DatabaseTransaction, tenant: Uuid) -> KabiPayResult<()> {
    if tenant.is_nil() {
        return Err(KabiPayError::Unauthorised);
    }
    // Keep the existing timeout for later advisory/row locks in this transaction,
    // as well as the authority's fixed input table locks.
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "SET LOCAL lock_timeout='15s'",
    ))
    .await?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT hrms_lock_payroll_inputs_v1($1)",
        [tenant.into()],
    ))
    .await?;
    Ok(())
}

pub async fn fingerprint<C: ConnectionTrait>(db: &C, tenant: Uuid) -> KabiPayResult<String> {
    if tenant.is_nil() {
        return Err(KabiPayError::Unauthorised);
    }
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT hrms_payroll_input_fingerprint_v1($1) AS fingerprint",
            [tenant.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("Payroll snapshot authority unavailable".into()))?;
    let digest = row.try_get::<String>("", "fingerprint")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(KabiPayError::Internal(
            "Payroll snapshot authority returned an invalid fingerprint".into(),
        ));
    }
    Ok(digest)
}
