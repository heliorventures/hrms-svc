//! Cross-module financial serialization, shared with HR separation submission.
use crate::KabiPayResult;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

pub async fn acquire(tx: &DatabaseTransaction, tenant: Uuid) -> KabiPayResult<()> {
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("financial:{tenant}").into()],
    ))
    .await?;
    Ok(())
}
