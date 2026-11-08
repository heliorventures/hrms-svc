use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::fnf_settlement;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, QueryFilter,
    QuerySelect, Statement,
};
use uuid::Uuid;

pub(crate) async fn ensure_draft(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    separation: Uuid,
) -> KabiPayResult<fnf_settlement::Model> {
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO fnf_settlement(id,tenant_id,separation_id,status,created_at,updated_at) VALUES($1,$2,$3,'DRAFT',NOW(),NOW()) ON CONFLICT(tenant_id,separation_id) DO NOTHING",
        [Uuid::new_v4().into(),tenant.into(),separation.into()])).await?;
    find(tx, tenant, separation, true)
        .await?
        .ok_or_else(|| KabiPayError::Internal("Final settlement could not be prepared".into()))
}

pub(crate) async fn find<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    separation: Uuid,
    lock: bool,
) -> KabiPayResult<Option<fnf_settlement::Model>> {
    let query = fnf_settlement::Entity::find()
        .filter(fnf_settlement::Column::TenantId.eq(tenant))
        .filter(fnf_settlement::Column::SeparationId.eq(separation));
    let query = if lock { query.lock_exclusive() } else { query };
    Ok(query.one(db).await?)
}
