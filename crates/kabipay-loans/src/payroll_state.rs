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
    check_disabled(tx,tenant,enabled).await?;
    let config = store::one(
        tx,
        "SELECT currency FROM kabipay_ops.tenant WHERE id=$1",
        vec![tenant.into()],
    )
    .await?
    .ok_or(LoanModuleError::PolicyUnavailable)?;
    let currency: Option<String> = config.try_get("", "currency")?;
    state(tx,tenant,enabled,currency).await
}

async fn check_disabled(tx:&DatabaseTransaction,tenant:Uuid,enabled:bool)->LoanResult<()> {
    // Disabling a product must not make an existing loan disappear from payroll review.
    if !enabled
        && store::one(
            tx,
            "SELECT a.id FROM loan_account a WHERE a.tenant_id=$1 AND (a.state='OPEN' OR EXISTS(SELECT 1 FROM loan_unapplied_credit c WHERE c.tenant_id=a.tenant_id AND c.loan_id=a.id AND c.remaining_amount>0)) LIMIT 1",
            vec![tenant.into()],
        )
        .await?
        .is_some()
    {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    Ok(())
}

async fn state(tx:&DatabaseTransaction,tenant:Uuid,enabled:bool,currency:Option<String>)->LoanResult<PayrollLoanState> {
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

/// The foreign payroll path uses only the private current source-runtime proof;
/// company loan rows remain in their separate source database.
pub async fn payroll_loan_state_with_actor(tx:&DatabaseTransaction,actor:&crate::LoanActorScope)->LoanResult<PayrollLoanState> {
 if let Some(entitlements)=actor.organization_entitlements.as_ref() {
  entitlements.require_tenant(actor.tenant_id)?;
  let enabled=entitlements.allows("LOANS");
  check_disabled(tx,actor.tenant_id,enabled).await?;
  state(tx,actor.tenant_id,enabled,entitlements.currency().map(str::to_owned)).await
 }else{payroll_loan_state(tx,actor.tenant_id).await}
}
