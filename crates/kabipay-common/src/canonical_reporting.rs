//! Canonical installed PRIMARY relationships take precedence at decision time.
//! A current foreign manager deliberately resolves to None, never legacy fallback.
use crate::KabiPayResult;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::HashMap;
use uuid::Uuid;

pub async fn manager<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    subject: Uuid,
    legacy: Option<Uuid>,
) -> KabiPayResult<Option<Uuid>> {
    manager_with_lock(db,tenant,subject,legacy,true).await
}

/// Eligibility reads take a bounded snapshot; they neither mutate a workflow
/// nor require UPDATE privileges merely to read the canonical relationship.
pub async fn manager_snapshot<C:ConnectionTrait+Sync>(db:&C,tenant:Uuid,subject:Uuid,legacy:Option<Uuid>)->KabiPayResult<Option<Uuid>>{
    manager_with_lock(db,tenant,subject,legacy,false).await
}
async fn manager_with_lock<C:ConnectionTrait+Sync>(db:&C,tenant:Uuid,subject:Uuid,legacy:Option<Uuid>,lock:bool)->KabiPayResult<Option<Uuid>>{
    let query=format!("SELECT manager_employee_id FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id=$2 AND relationship_type='PRIMARY' AND status='ACTIVE' AND effective_from<=clock_timestamp() AND (effective_to IS NULL OR effective_to>clock_timestamp()){}",if lock{" FOR SHARE"}else{""});
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,query,[tenant.into(),subject.into()])).await?;
    row.map(|r| r.try_get::<Option<Uuid>>("", "manager_employee_id"))
        .transpose()
        .map(|r| r.unwrap_or(legacy))
        .map_err(Into::into)
}

/// Queue inputs are bounded by their caller. Preserve the existing pure batch
/// eligibility evaluator after replacing only the effective manager reference.
pub async fn apply_managers<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    employees: &mut HashMap<Uuid, employee::Model>,
) -> KabiPayResult<()> {
    if employees.is_empty() {
        return Ok(());
    }
    let mut ids: Vec<_> = employees.keys().copied().collect();
    ids.sort_unstable();
    let slots = (2..=ids.len() + 1)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![tenant.into()];
    values.extend(ids.into_iter().map(Into::into));
    let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT subject_employee_id,manager_employee_id FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id IN ({slots}) AND relationship_type='PRIMARY' AND status='ACTIVE' AND effective_from<=clock_timestamp() AND (effective_to IS NULL OR effective_to>clock_timestamp())"),values)).await?;
    for row in rows {
        let subject: Uuid = row.try_get("", "subject_employee_id")?;
        if let Some(employee) = employees.get_mut(&subject) {
            employee.reporting_manager_id = row.try_get("", "manager_employee_id")?;
        }
    }
    Ok(())
}
