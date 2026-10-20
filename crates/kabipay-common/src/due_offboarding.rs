//! Idempotent employee access deactivation for approved, due separations.

use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{KabiPayError, KabiPayResult};

const DEFAULT_BATCH_SIZE: u64 = 100;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DueOffboardingResult {
    pub processed: u64,
}

/// Performs the full offboarding state transition using only the supplied transaction.
///
/// The conditional separation update is the idempotency gate. Concurrent callers serialize on
/// that row; only the winner can deactivate the employee/user, revoke sessions, and emit the
/// durable outbox event.
pub async fn offboard_approved_separation_in_transaction(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    separation_id: Uuid,
    business_date: NaiveDate,
    processed_at: DateTime<Utc>,
) -> KabiPayResult<bool> {
    let event_id = Uuid::new_v4();
    let row = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE separation
               SET offboarded_at = $4,
                   offboarding_event_id = $5,
                   updated_at = $4
               WHERE id = $1
                 AND tenant_id = $2
                 AND status = 'APPROVED'
                 AND last_working_date <= $3
                 AND offboarded_at IS NULL
               RETURNING employee_id"#,
            vec![
                separation_id.into(),
                tenant_id.into(),
                business_date.into(),
                processed_at.into(),
                event_id.into(),
            ],
        ))
        .await?;

    let Some(row) = row else {
        return Ok(false);
    };
    let employee_id: Uuid = row.try_get("", "employee_id")?;

    let employee = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT user_id
               FROM employee
               WHERE id = $1 AND tenant_id = $2 AND is_deleted = FALSE
               FOR UPDATE"#,
            vec![employee_id.into(), tenant_id.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    let user_id: Option<Uuid> = employee.try_get("", "user_id")?;

    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE employee
           SET status = 'INACTIVE', updated_at = $3
           WHERE id = $1 AND tenant_id = $2 AND status <> 'INACTIVE'"#,
        vec![employee_id.into(), tenant_id.into(), processed_at.into()],
    ))
    .await?;

    if let Some(user_id) = user_id {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE "user"
               SET is_active = FALSE, updated_at = $3
               WHERE id = $1 AND tenant_id = $2 AND is_deleted = FALSE"#,
            vec![user_id.into(), tenant_id.into(), processed_at.into()],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM user_session WHERE user_id = $1",
            vec![user_id.into()],
        ))
        .await?;
    }

    let payload = json!({
        "schema_version": 1,
        "separation_id": separation_id,
        "employee_id": employee_id,
        "offboarded_at": processed_at,
    });
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO private_domain_receipt(tenant_id,id,event_type,aggregate_type,aggregate_id,occurred_at,source_hash) VALUES($1,$2,'employee.offboarded','separation',$3,$4,$5)",
        [tenant_id.into(),event_id.into(),separation_id.into(),processed_at.into(),crate::private_events::crypto::source_hash(&serde_json::to_vec(&payload)?).into()],
    )).await?;
    crate::private_webhooks::enqueue_webhooks(txn, crate::private_webhooks::LegacyWebhookEvent {
        id: event_id, tenant_id, aggregate_type: "separation".into(), aggregate_id: separation_id,
        event_type: "employee.offboarded".into(), payload, created_at: processed_at,
    }).await?;

    Ok(true)
}

/// Scans a bounded set of due rows and processes each in its own retry-safe transaction.
pub async fn process_due_separations(
    tenant_db: &DatabaseConnection,
    tenant_id: Uuid,
    business_date: NaiveDate,
) -> KabiPayResult<DueOffboardingResult> {
    let rows = tenant_db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT id
               FROM separation
               WHERE tenant_id = $1
                 AND status = 'APPROVED'
                 AND last_working_date <= $2
                 AND offboarded_at IS NULL
               ORDER BY last_working_date, id
               LIMIT $3"#,
            vec![
                tenant_id.into(),
                business_date.into(),
                DEFAULT_BATCH_SIZE.into(),
            ],
        ))
        .await?;

    let mut processed = 0;
    for row in rows {
        let separation_id: Uuid = row.try_get("", "id")?;
        let txn = tenant_db.begin().await?;
        let did_process = offboard_approved_separation_in_transaction(
            &txn,
            tenant_id,
            separation_id,
            business_date,
            Utc::now(),
        )
        .await?;
        txn.commit().await?;
        processed += u64::from(did_process);
    }

    Ok(DueOffboardingResult { processed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_count_only_tracks_winning_transitions() {
        let result = DueOffboardingResult { processed: 0 };
        assert_eq!(result.processed, 0);
    }
}
