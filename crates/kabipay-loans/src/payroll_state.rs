//! Private coordination state; no account data or employee notes cross the boundary.
use crate::{repository as store, LoanModuleError, LoanResult};
use kabipay_common::entitlements::Entitlements;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub struct PayrollLoanState {
    pub enabled: bool,
    pub currency: Option<String>,
    pub fingerprint: String,
}

pub async fn payroll_loan_state(
    tx: &DatabaseTransaction,
    tenant: Uuid,
) -> LoanResult<PayrollLoanState> {
    if tenant.is_nil() {
        return Err(LoanModuleError::Forbidden);
    }
    let enabled = Entitlements::load(tx, tenant).await?.allows("LOANS");
    // Disabling a product must not make an existing loan disappear from payroll review.
    if !enabled
        && store::one(
            tx,
            "SELECT id FROM loan_account WHERE tenant_id=$1 AND state='OPEN' LIMIT 1",
            vec![tenant.into()],
        )
        .await?
        .is_some()
    {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    let config = store::one(
        tx,
        "SELECT currency FROM kabipay_ops.tenant WHERE id=$1",
        vec![tenant.into()],
    )
    .await?
    .ok_or(LoanModuleError::PolicyUnavailable)?;
    let currency: Option<String> = config.try_get("", "currency")?;
    let rows=store::all(tx,"SELECT employee_id,revision FROM loan_employee_state WHERE tenant_id=$1 ORDER BY employee_id",vec![tenant.into()]).await?;
    let revisions: Vec<(Uuid, i64)> = rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get("", "employee_id")?,
                row.try_get("", "revision")?,
            ))
        })
        .collect::<Result<_, sea_orm::DbErr>>()?;
    let fingerprint = crate::command_hash(&(tenant, enabled, &currency, revisions))?;
    Ok(PayrollLoanState {
        enabled,
        currency,
        fingerprint,
    })
}
