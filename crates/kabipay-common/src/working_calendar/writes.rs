use crate::KabiPayResult;
use chrono::Utc;
use kabipay_db_entities::tenant::d0027_communication_audit::audit_log;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, Set, Statement};
use uuid::Uuid;

/// All calendar/location writers take this transaction lock before row locks.
pub async fn lock_calendar<C: ConnectionTrait>(txn: &C, tenant: Uuid) -> KabiPayResult<()> {
    txn.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("working_calendar:{tenant}").into()],
    ))
    .await?;
    Ok(())
}

/// Call inside the same locked transaction as a holiday or roster configuration change.
pub async fn bump_calendar_revision<C: ConnectionTrait>(
    txn: &C,
    tenant: Uuid,
) -> KabiPayResult<()> {
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE working_calendar_profile SET revision = revision + 1, updated_at = NOW() WHERE tenant_id = $1", [tenant.into()])).await?;
    Ok(())
}

pub async fn audit_calendar<C: ConnectionTrait>(
    txn: &C,
    tenant: Uuid,
    actor: Uuid,
    entity: &str,
    id: Uuid,
    action: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) -> KabiPayResult<()> {
    audit_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant),
        user_id: Set(Some(actor)),
        entity_type: Set(entity.into()),
        entity_id: Set(Some(id)),
        action: Set(action.into()),
        before_state: Set(before),
        after_state: Set(after),
        ip_address: Set(None),
        user_agent: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(txn)
    .await?;
    Ok(())
}
