//! Employee-owned durable approval identity; no Loan or FNF financial storage access.
use kabipay_common::{
    context::ClientClaims, finance_contract::HrOperationState, KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use kabipay_common::finance_contract::{ExitOperationStatus as ExitOperation, ExitSubject};

pub fn subject(row: &separation::Model) -> KabiPayResult<ExitSubject> {
    let bytes = serde_json::to_vec(&(
        row.tenant_id,
        row.id,
        row.employee_id,
        &row.status,
        row.last_working_date,
        row.resignation_date,
        row.updated_at,
    ))
    .map_err(|_| KabiPayError::Internal("Exit version serialization failed".into()))?;
    Ok(ExitSubject {
        separation_id: row.id,
        employee_id: row.employee_id,
        last_working_date: row.last_working_date,
        status: row.status.clone(),
        fingerprint: format!("{:x}", Sha256::digest(bytes)),
    })
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    lock: bool,
) -> KabiPayResult<Option<ExitOperation>> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT id,separation_id,employee_id,actor_id,review_id,separation_fingerprint,payload_hash,authorization_id,state FROM employee_exit_operation WHERE tenant_id=$1 AND id=$2{suffix}"),[tenant.into(),id.into()])).await?;
    row.map(|r| {
        Ok(ExitOperation {
            id: r.try_get("", "id")?,
            separation_id: r.try_get("", "separation_id")?,
            employee_id: r.try_get("", "employee_id")?,
            actor_id: r.try_get("", "actor_id")?,
            review_id: r.try_get("", "review_id")?,
            separation_fingerprint: r.try_get("", "separation_fingerprint")?,
            payload_hash: r.try_get("", "payload_hash")?,
            authorization_id: r.try_get("", "authorization_id")?,
            state: serde_json::from_value(serde_json::Value::String(r.try_get("", "state")?))
                .map_err(|_| {
                    KabiPayError::Internal("Exit operation state requires reconciliation".into())
                })?,
        })
    })
    .transpose()
}

pub async fn register(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    row: &separation::Model,
    review: Uuid,
) -> KabiPayResult<ExitOperation> {
    if claims.tenant_id != tenant
        || row.tenant_id != tenant
        || review.is_nil()
        || kabipay_common::client_data_scope::data_scope_from_claims(
            Some(claims),
            "onboarding:manage",
        )? != kabipay_common::context::ScopeType::All
    {
        return Err(KabiPayError::Forbidden(
            "Exit approval requires ALL HR scope".into(),
        ));
    }
    let subject = subject(row)?;
    let payload_hash = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(tenant, claims.sub, review, &subject)).map_err(|_| {
                KabiPayError::Internal("Exit operation serialization failed".into())
            })?
        )
    );
    let tx = db.begin().await?;
    let current = separation::Entity::find_by_id(row.id)
        .filter(separation::Column::TenantId.eq(tenant))
        .lock_exclusive()
        .one(&tx)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "separation",
            id: row.id.to_string(),
        })?;
    if self::subject(&current)?.fingerprint != subject.fingerprint {
        return Err(KabiPayError::Conflict(
            "Separation changed before HR approval registration".into(),
        ));
    }
    let prior=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM employee_exit_operation WHERE tenant_id=$1 AND separation_id=$2 AND (state='REGISTERED' OR (review_id=$3 AND actor_id=$4 AND state='COMMITTED')) ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
        [tenant.into(),row.id.into(),review.into(),claims.sub.into()])).await?;
    if let Some(prior) = prior {
        let operation = find(&tx, tenant, prior.try_get("", "id")?, false)
            .await?
            .ok_or_else(|| KabiPayError::Internal("Exit operation disappeared".into()))?;
        if operation.review_id != review
            || operation.actor_id != claims.sub
            || (operation.state == HrOperationState::Registered
                && operation.payload_hash != payload_hash)
        {
            return Err(KabiPayError::Conflict(
                "An exit approval is pending; reconcile it before a new review".into(),
            ));
        }
        tx.commit().await?;
        return Ok(operation);
    }
    if row.status != "PENDING" {
        return Err(KabiPayError::Validation("Separation is not pending".into()));
    }
    let id = Uuid::new_v4();
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_exit_operation(id,tenant_id,separation_id,employee_id,actor_id,review_id,separation_fingerprint,payload_hash,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'REGISTERED')",
        [id.into(),tenant.into(),row.id.into(),row.employee_id.into(),claims.sub.into(),review.into(),subject.fingerprint.into(),payload_hash.into()])).await?;
    let operation = find(&tx, tenant, id, false)
        .await?
        .ok_or_else(|| KabiPayError::Internal("Exit operation was not registered".into()))?;
    tx.commit().await?;
    Ok(operation)
}

pub async fn record_terminal(
    db: &DatabaseTransaction,
    tenant: Uuid,
    operation: &ExitOperation,
    state: HrOperationState,
    authorization: Option<Uuid>,
) -> KabiPayResult<()> {
    let (status, event) = match state {
        HrOperationState::Committed => ("COMMITTED", "EXIT_APPROVAL_COMMITTED"),
        HrOperationState::Cancelled => ("CANCELLED", "EXIT_APPROVAL_CANCELLED"),
        HrOperationState::Registered => {
            return Err(KabiPayError::Validation(
                "A terminal HR outcome is required".into(),
            ))
        }
    };
    let changed=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE employee_exit_operation SET state=$3,authorization_id=$4,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND state='REGISTERED'",
        [tenant.into(),operation.id.into(),status.into(),authorization.into()])).await?;
    if changed.rows_affected() != 1 {
        return Err(KabiPayError::Conflict(
            "Exit approval operation has already resolved".into(),
        ));
    }
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_finance_outbox(id,tenant_id,operation_id,event_type,payload) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,operation_id,event_type) DO NOTHING",
        [Uuid::new_v4().into(),tenant.into(),operation.id.into(),event.into(),serde_json::json!({"operationId":operation.id,"separationId":operation.separation_id,"reviewId":operation.review_id,"authorizationId":authorization,"payloadHash":operation.payload_hash}).into()])).await?;
    Ok(())
}

pub async fn cancel(db: &DatabaseConnection, tenant: Uuid, id: Uuid) -> KabiPayResult<()> {
    let tx = db.begin().await?;
    let operation = find(&tx, tenant, id, true)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "exit_operation",
            id: id.to_string(),
        })?;
    if operation.state == HrOperationState::Registered {
        record_terminal(
            &tx,
            tenant,
            &operation,
            HrOperationState::Cancelled,
            operation.authorization_id,
        )
        .await?;
    } else if operation.state == HrOperationState::Committed {
        return Err(KabiPayError::Conflict(
            "Committed HR approval cannot be cancelled".into(),
        ));
    }
    tx.commit().await?;
    Ok(())
}

pub async fn for_separation<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    separation: Uuid,
    actor: Uuid,
) -> KabiPayResult<Option<ExitOperation>> {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM employee_exit_operation WHERE tenant_id=$1 AND separation_id=$2 AND actor_id=$3 AND state IN ('REGISTERED','COMMITTED') ORDER BY created_at DESC LIMIT 1",
        [tenant.into(), separation.into(), actor.into()])).await?;
    match row {
        Some(row) => find(db, tenant, row.try_get("", "id")?, false).await,
        None => Ok(None),
    }
}

/// Company HR recovery view, including another HR user's pending operation.
pub async fn latest_for_separation<C: ConnectionTrait>(
    db: &C, tenant: Uuid, separation: Uuid,
) -> KabiPayResult<Option<ExitOperation>> {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM employee_exit_operation WHERE tenant_id=$1 AND separation_id=$2 AND state IN ('REGISTERED','COMMITTED') ORDER BY created_at DESC,id DESC LIMIT 1",
        [tenant.into(), separation.into()])).await?;
    match row {
        Some(row) => find(db, tenant, row.try_get("", "id")?, false).await,
        None => Ok(None),
    }
}

pub async fn cancel_registered_for_separation(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    separation: Uuid,
) -> KabiPayResult<()> {
    let rows = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM employee_exit_operation WHERE tenant_id=$1 AND separation_id=$2 AND state='REGISTERED' FOR UPDATE",
        [tenant.into(), separation.into()])).await?;
    for row in rows {
        let operation = find(tx, tenant, row.try_get("", "id")?, false)
            .await?
            .ok_or_else(|| KabiPayError::Internal("Registered approval disappeared".into()))?;
        record_terminal(
            tx,
            tenant,
            &operation,
            HrOperationState::Cancelled,
            operation.authorization_id,
        )
        .await?;
    }
    Ok(())
}
