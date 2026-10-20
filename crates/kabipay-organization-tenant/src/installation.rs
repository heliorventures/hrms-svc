use crate::{statement, unavailable};
use kabipay_common::KabiPayResult;
use kabipay_organization_protocol::*;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;

pub async fn install_actor(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    a: &ActorReference,
) -> KabiPayResult<()> {
    if a.principal_id.is_nil()
        || a.identity_version <= 0
        || a.display_name.trim().is_empty()
        || a.display_name.chars().count() > 255
    {
        return Err(unavailable());
    }
    let changed=tx.execute(statement("INSERT INTO actor_ref(tenant_id,principal_id,display_name,identity_version,last_verified_at) VALUES($1,$2,$3,$4,now()) ON CONFLICT(tenant_id,principal_id) DO UPDATE SET display_name=excluded.display_name,identity_version=excluded.identity_version,last_verified_at=now() WHERE actor_ref.identity_version<=excluded.identity_version",vec![tenant.into(),a.principal_id.into(),a.display_name.clone().into(),a.identity_version.into()])).await?;
    if changed.rows_affected() != 1 {
        return Err(unavailable());
    }
    Ok(())
}

/// Business commands already have an installed grant/assignment actor. Read it
/// without taking a mutable display-row lock before domain locks.
pub(crate) async fn require_actor(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    a: &ActorReference,
) -> KabiPayResult<()> {
    if a.identity_version <= 0
        || a.display_name.trim().is_empty()
        || a.display_name.chars().count() > 255
    {
        return Err(unavailable());
    }
    let row = tx
        .query_one(statement(
            "SELECT identity_version FROM actor_ref WHERE tenant_id=$1 AND principal_id=$2",
            vec![tenant.into(), a.principal_id.into()],
        ))
        .await?
        .ok_or_else(unavailable)?;
    if row.try_get::<i64>("", "identity_version")? > a.identity_version {
        return Err(unavailable());
    }
    Ok(())
}
/// Display refresh is best effort and never waits behind metadata installation.
/// It runs after every business write, so its lock cannot invert a later domain lock.
pub(crate) async fn refresh_actor(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    a: &ActorReference,
) -> KabiPayResult<()> {
    tx.execute(statement("WITH unlocked AS (SELECT principal_id FROM actor_ref WHERE tenant_id=$1 AND principal_id=$2 AND identity_version<=$4 FOR UPDATE SKIP LOCKED) UPDATE actor_ref r SET display_name=$3,identity_version=$4,last_verified_at=clock_timestamp() FROM unlocked u WHERE r.tenant_id=$1 AND r.principal_id=u.principal_id",vec![tenant.into(),a.principal_id.into(),a.display_name.clone().into(),a.identity_version.into()])).await?;
    Ok(())
}

pub async fn install_grant(
    tx: &DatabaseTransaction,
    p: &RuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    let _: crate::EmptyPayload = serde_json::from_value(c.payload.clone())?;
    let g = p.grant.as_ref().ok_or_else(unavailable)?;
    if g.tenant_id != p.tenant_id
        || g.organization_id != p.organization_id
        || g.membership_id != p.membership_id
        || g.binding_id != c.resource_id
        || g.grant_version != c.expected_version
        || g.membership_version != p.membership_version
        || g.actor.principal_id != g.principal_id
        || g.configuration_hash.len() != 64
        || g.capabilities.is_empty()
    {
        return Err(unavailable());
    }
    install_actor(tx, p.tenant_id, &g.actor).await?;
    if let Some(existing)=tx.query_one(statement("SELECT grant_version,membership_version,configuration_hash,organization_id,membership_id,principal_id FROM organization_grant_installation WHERE tenant_id=$1 AND binding_id=$2 FOR UPDATE",vec![p.tenant_id.into(),g.binding_id.into()])).await? {
        let version:i64=existing.try_get("","grant_version")?;
        if existing.try_get::<Uuid>("","organization_id")?!=g.organization_id || existing.try_get::<Uuid>("","membership_id")?!=g.membership_id || existing.try_get::<Uuid>("","principal_id")?!=g.principal_id {return Err(unavailable());}
        if version>g.grant_version || g.membership_version<existing.try_get::<i64>("","membership_version")? || (version==g.grant_version && existing.try_get::<String>("","configuration_hash")?!=g.configuration_hash && g.membership_version<=existing.try_get::<i64>("","membership_version")?){return Err(unavailable());}
    }
    tx.execute(statement("INSERT INTO organization_grant_installation(tenant_id,binding_id,organization_id,membership_id,principal_id,grant_version,membership_version,capabilities,configuration_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(tenant_id,binding_id) DO UPDATE SET organization_id=excluded.organization_id,membership_id=excluded.membership_id,principal_id=excluded.principal_id,grant_version=excluded.grant_version,membership_version=excluded.membership_version,capabilities=excluded.capabilities,configuration_hash=excluded.configuration_hash,installed_at=now(),status='ACTIVE'",vec![p.tenant_id.into(),g.binding_id.into(),g.organization_id.into(),g.membership_id.into(),g.principal_id.into(),g.grant_version.into(),g.membership_version.into(),serde_json::to_value(&g.capabilities)?.into(),g.configuration_hash.clone().into()])).await?;
    crate::queue_bootstrap(tx, p.tenant_id, g.organization_id).await?;
    Ok(
        serde_json::json!({"installed":true,"grant_version":g.grant_version,"configuration_hash":g.configuration_hash,"source_bootstrap":"PENDING"}),
    )
}

pub async fn install_relationship(
    tx: &DatabaseTransaction,
    p: &RuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    let _: crate::EmptyPayload = serde_json::from_value(c.payload.clone())?;
    let r = p.relationship.as_ref().ok_or_else(unavailable)?;
    if r.tenant_id != p.tenant_id
        || r.organization_id != p.organization_id
        || r.membership_id != p.membership_id
        || r.id != c.resource_id
        || r.version != c.expected_version
        || r.graph_version <= 0
        || r.configuration_hash.len() != 64
        || r.subject_principal_id == r.manager_principal_id
        || r.subject_employment_ref_id == r.manager_employment_ref_id
        || r.effective_to.is_some_and(|e| e <= r.effective_from)
    {
        return Err(unavailable());
    }
    if (r.manager_tenant_id == p.tenant_id) != r.manager_employee_id.is_some()
        || r.manager_employee_id == Some(r.subject_employee_id)
    {
        return Err(unavailable());
    }
    {
        tx.query_all(statement("SELECT c.id FROM review_cycle c WHERE c.tenant_id=$1 AND EXISTS(SELECT 1 FROM performance_participant p WHERE p.tenant_id=c.tenant_id AND p.review_cycle_id=c.id AND p.employee_id=$2) ORDER BY c.id FOR UPDATE OF c",vec![p.tenant_id.into(),r.subject_employee_id.into()])).await?;
    }
    let mut employees = vec![r.subject_employee_id];
    if let Some(manager) = r.manager_employee_id {
        employees.push(manager);
    }
    employees.sort_unstable();
    employees.dedup();
    let slots = (2..=employees.len() + 1)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![p.tenant_id.into()];
    values.extend(employees.into_iter().map(Into::into));
    tx.query_all(statement(
        &format!(
            "SELECT id FROM employee WHERE tenant_id=$1 AND id IN ({slots}) ORDER BY id FOR SHARE"
        ),
        values,
    ))
    .await?;
    if let Some(manager) = r.manager_employee_id {
        if tx.query_one(statement("SELECT 1 AS n FROM employee WHERE tenant_id=$1 AND id=$2 AND ($3 OR (NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE'))) FOR SHARE",vec![p.tenant_id.into(),manager.into(),(r.status=="ENDED").into()])).await?.is_none(){return Err(unavailable());}
    }
    for a in &r.actors {
        install_actor(tx, p.tenant_id, a).await?;
    }
    if !r
        .actors
        .iter()
        .any(|a| a.principal_id == r.subject_principal_id)
        || !r
            .actors
            .iter()
            .any(|a| a.principal_id == r.manager_principal_id)
    {
        return Err(unavailable());
    }
    if tx
        .query_one(statement(
            "SELECT 1 AS n FROM employee WHERE tenant_id=$1 AND id=$2 AND ($3 OR NOT is_deleted) FOR SHARE",
            vec![p.tenant_id.into(), r.subject_employee_id.into(),(r.status=="ENDED").into()],
        ))
        .await?
        .is_none()
    {
        return Err(unavailable());
    }
    {
        tx.query_all(statement("SELECT id FROM performance_participant WHERE tenant_id=$1 AND employee_id=$2 ORDER BY id FOR UPDATE",vec![p.tenant_id.into(),r.subject_employee_id.into()])).await?;
    }
    let mut was_current_primary = false;
    if let Some(old)=tx.query_one(statement("SELECT version,graph_version,configuration_hash,(relationship_type='PRIMARY' AND status='ACTIVE' AND effective_from<=clock_timestamp() AND (effective_to IS NULL OR effective_to>clock_timestamp())) AS current_primary FROM management_assignment WHERE tenant_id=$1 AND id=$2 FOR UPDATE",vec![p.tenant_id.into(),r.id.into()])).await? {
        was_current_primary=old.try_get("","current_primary")?;
        let v:i64=old.try_get("","version")?;
        if v>r.version || old.try_get::<i64>("","graph_version")?>r.graph_version || (v==r.version && old.try_get::<Option<String>>("","configuration_hash")?.as_deref()!=Some(r.configuration_hash.as_str())){return Err(unavailable());}
    }
    let changed=tx.execute(statement("INSERT INTO management_assignment(id,tenant_id,organization_id,membership_id,subject_employee_id,subject_principal_id,manager_principal_id,subject_employment_ref_id,manager_employment_ref_id,manager_tenant_id,relationship_type,status,effective_from,effective_to,version,graph_version,configuration_hash,manager_employee_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) ON CONFLICT(id) DO UPDATE SET subject_employee_id=excluded.subject_employee_id,subject_principal_id=excluded.subject_principal_id,manager_principal_id=excluded.manager_principal_id,subject_employment_ref_id=excluded.subject_employment_ref_id,manager_employment_ref_id=excluded.manager_employment_ref_id,manager_tenant_id=excluded.manager_tenant_id,manager_employee_id=excluded.manager_employee_id,relationship_type=excluded.relationship_type,status=excluded.status,effective_from=excluded.effective_from,effective_to=excluded.effective_to,version=excluded.version,graph_version=excluded.graph_version,configuration_hash=excluded.configuration_hash,updated_at=now() WHERE management_assignment.tenant_id=excluded.tenant_id AND management_assignment.organization_id=excluded.organization_id AND management_assignment.membership_id=excluded.membership_id",vec![r.id.into(),p.tenant_id.into(),r.organization_id.into(),r.membership_id.into(),r.subject_employee_id.into(),r.subject_principal_id.into(),r.manager_principal_id.into(),r.subject_employment_ref_id.into(),r.manager_employment_ref_id.into(),r.manager_tenant_id.into(),r.relationship_type.clone().into(),r.status.clone().into(),r.effective_from.into(),r.effective_to.into(),r.version.into(),r.graph_version.into(),r.configuration_hash.clone().into(),r.manager_employee_id.into()])).await?;
    if changed.rows_affected() != 1 {
        return Err(unavailable());
    }
    tx.execute(statement(
        "DELETE FROM assignment_capability WHERE tenant_id=$1 AND assignment_id=$2",
        vec![p.tenant_id.into(), r.id.into()],
    ))
    .await?;
    for capability in &r.capabilities {
        tx.execute(statement("INSERT INTO assignment_capability(tenant_id,assignment_id,capability_code) VALUES($1,$2,$3)",vec![p.tenant_id.into(),r.id.into(),capability.clone().into()])).await?;
    }
    if matches!(r.relationship_type.as_str(), "PRIMARY" | "REVIEWER")
        && r.status == "ACTIVE"
        && r.capabilities
            .iter()
            .any(|c| c == "performance:evaluate" || c == "feedback:write")
    {
        tx.query_all(statement("SELECT id FROM performance_participant WHERE tenant_id=$1 AND employee_id=$2 AND NOT is_excluded AND manager_submitted_at IS NULL ORDER BY id FOR UPDATE",vec![p.tenant_id.into(),r.subject_employee_id.into()])).await?;
        tx.execute(statement("INSERT INTO organization_review_manager(tenant_id,participant_id,principal_id,assignment_id,assignment_version) SELECT tenant_id,id,$3,$4,$5 FROM performance_participant WHERE tenant_id=$1 AND employee_id=$2 AND NOT is_excluded AND manager_submitted_at IS NULL ON CONFLICT(tenant_id,participant_id,assignment_id) DO UPDATE SET principal_id=excluded.principal_id,assignment_id=excluded.assignment_id,assignment_version=excluded.assignment_version",vec![p.tenant_id.into(),r.subject_employee_id.into(),r.manager_principal_id.into(),r.id.into(),r.version.into()])).await?;
    }
    if r.relationship_type == "PRIMARY" || was_current_primary {
        rebind_pending_primary(
            tx,
            p.tenant_id,
            r.subject_employee_id,
            r.id,
            was_current_primary,
        )
        .await?;
    }
    crate::queue_bootstrap(tx, p.tenant_id, r.organization_id).await?;
    Ok(
        serde_json::json!({"installed":true,"version":r.version,"graph_version":r.graph_version,"configuration_hash":r.configuration_hash,"source_bootstrap":"PENDING"}),
    )
}

/// The participant FK is a pending local fallback, never a retained authority
/// from an ended canonical PRIMARY. Future installs do not rewrite it early.
async fn rebind_pending_primary(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
    assignment: Uuid,
    was_current: bool,
) -> KabiPayResult<()> {
    tx.execute(statement("UPDATE performance_participant p SET manager_employee_id=CASE WHEN EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=p.tenant_id AND a.subject_employee_id=p.employee_id AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())) THEN (SELECT a.manager_employee_id FROM management_assignment a WHERE a.tenant_id=p.tenant_id AND a.subject_employee_id=p.employee_id AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())) ELSE e.reporting_manager_id END FROM employee e WHERE p.tenant_id=$1 AND p.employee_id=$2 AND e.tenant_id=p.tenant_id AND e.id=p.employee_id AND p.manager_submitted_at IS NULL AND ($4 OR EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=$1 AND a.id=$3 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())))",vec![tenant.into(),employee.into(),assignment.into(),was_current.into()])).await?;
    Ok(())
}

#[cfg(test)]
#[path="installation_tests.rs"]
mod tests;
