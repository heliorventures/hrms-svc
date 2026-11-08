use crate::{repository as store, LoanModuleError, LoanResult};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub async fn due_financial_audits(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    first: u32,
) -> LoanResult<Vec<Uuid>> {
    if tenant.is_nil() || first == 0 || first > 100 {
        return Err(LoanModuleError::InvalidCommand);
    }
    store::synchronize(tx, tenant).await?;
    let rows = store::all(tx, r#"
        SELECT a.id FROM loan_account a
        LEFT JOIN loan_reconciliation_status s ON s.tenant_id=a.tenant_id AND s.id=a.id
        LEFT JOIN loan_reconciliation_run r ON r.tenant_id=s.tenant_id AND r.loan_id=s.id AND r.id=s.last_run_id
        WHERE a.tenant_id=$1 AND (s.id IS NULL OR s.checked_at<NOW()-INTERVAL '24 hours' OR r.account_version<>a.version
          OR EXISTS(SELECT 1 FROM loan_internal_outbox o WHERE o.tenant_id=a.tenant_id AND o.employee_id=a.employee_id AND o.delivered_at IS NULL AND o.available_at<=NOW() AND o.created_at>s.checked_at))
        ORDER BY s.checked_at NULLS FIRST,a.id LIMIT $2
    "#, vec![tenant.into(),i64::from(first).into()]).await?;
    rows.into_iter()
        .map(|row| Ok(row.try_get("", "id")?))
        .collect()
}

/// The private in-process consumer acknowledges only after every affected account was checked.
pub async fn acknowledge_financial_events(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    first: u32,
) -> LoanResult<u64> {
    if tenant.is_nil() || first == 0 || first > 100 {
        return Err(LoanModuleError::InvalidCommand);
    }
    store::synchronize(tx, tenant).await?;
    let rows = store::all(tx, r#"
        SELECT o.id FROM loan_internal_outbox o
        WHERE o.tenant_id=$1 AND o.delivered_at IS NULL AND o.available_at<=NOW()
          AND o.payload->>'employeeId'=o.employee_id::text AND (o.payload->>'revision')::bigint=o.aggregate_revision
          AND EXISTS(SELECT 1 FROM loan_employee_state e WHERE e.tenant_id=o.tenant_id AND e.employee_id=o.employee_id AND e.revision>=o.aggregate_revision)
          AND NOT EXISTS(SELECT 1 FROM loan_account a LEFT JOIN loan_reconciliation_status s ON s.tenant_id=a.tenant_id AND s.id=a.id
            LEFT JOIN loan_reconciliation_run r ON r.tenant_id=s.tenant_id AND r.loan_id=s.id AND r.id=s.last_run_id
            WHERE a.tenant_id=o.tenant_id AND a.employee_id=o.employee_id AND (s.id IS NULL OR s.checked_at<o.created_at OR r.account_version<>a.version))
        ORDER BY o.available_at,o.id LIMIT $2
    "#,vec![tenant.into(),i64::from(first).into()]).await?;
    let mut count = 0;
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        store::execute(tx,"INSERT INTO loan_consumer_receipt(id,tenant_id,event_id,consumer,processed_at) VALUES($1,$2,$3,'FINANCIAL_RECONCILIATION_V1',NOW()) ON CONFLICT(tenant_id,event_id,consumer) DO NOTHING",
            vec![Uuid::new_v4().into(),tenant.into(),id.into()]).await?;
        count += store::execute(tx,"UPDATE loan_internal_outbox SET delivered_at=NOW(),attempts=attempts+1 WHERE tenant_id=$1 AND id=$2 AND delivered_at IS NULL",vec![tenant.into(),id.into()]).await?;
    }
    Ok(count)
}
