use crate::{repository as store, LoanActorScope, LoanModuleError, LoanResult};
use kabipay_common::{
    client_data_scope::{resolve_employee_scope_filter_with_connection, EmployeeScopeFilter},
    context::{ClientViewerEmployee, ScopeType},
    entitlements::Entitlements,
};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) async fn entitled(tx: &DatabaseTransaction, actor: &LoanActorScope) -> LoanResult<()> {
    Entitlements::load(tx, actor.tenant_id)
        .await?
        .require("LOANS")?;
    Ok(())
}
pub(crate) async fn viewer(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
) -> LoanResult<Option<ClientViewerEmployee>> {
    let row=store::one(tx,"SELECT id,department_id FROM employee WHERE tenant_id=$1 AND user_id=$2 AND is_deleted=FALSE",vec![actor.tenant_id.into(),actor.user_id.into()]).await?;
    row.map(|r| {
        Ok(ClientViewerEmployee {
            employee_id: r.try_get("", "id")?,
            department_id: r.try_get("", "department_id")?,
        })
    })
    .transpose()
}
pub(crate) async fn filter(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
) -> LoanResult<EmployeeScopeFilter> {
    Ok(resolve_employee_scope_filter_with_connection(
        tx,
        actor.tenant_id,
        actor.scope,
        viewer(tx, actor).await?,
    )
    .await?)
}
pub(crate) async fn employee(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    employee: Uuid,
) -> LoanResult<()> {
    if !filter(tx, actor).await?.allows_employee(employee) {
        return Err(LoanModuleError::Forbidden);
    }
    let found = store::one(
        tx,
        "SELECT id FROM employee WHERE tenant_id=$1 AND id=$2 AND is_deleted=FALSE",
        vec![actor.tenant_id.into(), employee.into()],
    )
    .await?;
    if found.is_none() {
        return Err(LoanModuleError::NotFound);
    }
    Ok(())
}
pub(crate) fn company(actor: &LoanActorScope) -> LoanResult<()> {
    if actor.scope != ScopeType::All {
        return Err(LoanModuleError::Forbidden);
    }
    Ok(())
}
