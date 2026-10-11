//! Private delivery is isolated from tenant webhooks and the wildcard outbox.
use kabipay_common::{
    finance_contract::{FinanceAction, FinanceAuthorizationState, ServiceCaller},
    finance_transport, KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryAcknowledgement {
    operation_id: Uuid,
    state: FinanceAuthorizationState,
}

pub async fn deliver_due(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<u64> {
    // Claim with a short lease, then perform HTTP outside every database transaction.
    let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE employee_finance_outbox SET attempts=attempts+1,available_at=NOW()+INTERVAL '1 minute' WHERE tenant_id=$1 AND id IN (SELECT id FROM employee_finance_outbox WHERE tenant_id=$1 AND delivered_at IS NULL AND available_at<=NOW() ORDER BY available_at,id LIMIT 5 FOR UPDATE SKIP LOCKED) RETURNING id,operation_id,event_type,attempts",
        [tenant.into()])).await?;
    let mut delivered = 0;
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        let operation: Uuid = row.try_get("", "operation_id")?;
        let attempts: i32 = row.try_get("", "attempts")?;
        let event: String = row.try_get("", "event_type")?;
        let (action, expected) = match event.as_str() {
            "EXIT_APPROVAL_COMMITTED" => (
                FinanceAction::ConfirmExitApproval,
                FinanceAuthorizationState::Confirmed,
            ),
            "EXIT_APPROVAL_CANCELLED" => (
                FinanceAction::CancelExitApproval,
                FinanceAuthorizationState::Cancelled,
            ),
            _ => {
                return Err(KabiPayError::Internal(
                    "Private Finance event requires reconciliation".into(),
                ))
            }
        };
        let response = finance_transport::send(
            ServiceCaller::Employee,
            ServiceCaller::Payroll,
            tenant,
            None,
            action,
            serde_json::json!({"operationId":operation}),
        )
        .await;
        let confirmed = response
            .ok()
            .and_then(|value| serde_json::from_value::<DeliveryAcknowledgement>(value).ok())
            .is_some_and(|value| value.operation_id == operation && value.state == expected);
        if confirmed {
            let result = db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE employee_finance_outbox SET delivered_at=NOW(),last_error=NULL WHERE tenant_id=$1 AND id=$2 AND attempts=$3 AND delivered_at IS NULL",
                [tenant.into(),id.into(),attempts.into()])).await?;
            delivered += result.rows_affected();
        } else {
            let delay = 30i64
                .saturating_mul(2i64.saturating_pow(attempts.clamp(0, 7) as u32))
                .min(3600);
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE employee_finance_outbox SET available_at=NOW()+($4 * INTERVAL '1 second'),last_error='Finance confirmation unavailable or awaiting reconciliation' WHERE tenant_id=$1 AND id=$2 AND attempts=$3 AND delivered_at IS NULL",
                [tenant.into(),id.into(),attempts.into(),delay.into()])).await?;
        }
    }
    Ok(delivered)
}
