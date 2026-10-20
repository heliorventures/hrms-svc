use crate::{identity::SourceIdentity, store::*};
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;
pub(crate) async fn invite(
    tx: &DatabaseTransaction,
    org: Uuid,
    tenant: Uuid,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(tenant)?;
    one(tx,"SELECT id FROM kabipay_ops.tenant WHERE id=$1 AND status='ACTIVE' AND NOT is_deleted FOR UPDATE",vec![tenant.into()]).await?;
    if tx.query_one(sql("SELECT id FROM kabipay_ops.organization_company WHERE tenant_id=$1 AND (status IN ('ACTIVE','SUSPENDED','DETACHING') OR (organization_id=$2 AND status='PENDING'))",vec![tenant.into(),org.into()])).await?.is_some(){return Err(conflict());}
    let id = Uuid::new_v4();
    tx.execute(sql("INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id,effective_from) VALUES($1,$2,$3,NOW())",vec![id.into(),org.into(),tenant.into()])).await?;
    Ok((id, "PENDING", 1))
}
pub(crate) async fn accept(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    membership: Uuid,
    version: i64,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(membership)?;
    if !actor.may_accept_company() || version <= 0 || version == i64::MAX {
        return Err(denied());
    }
    one(tx,"SELECT id FROM kabipay_ops.tenant WHERE id=$1 AND status='ACTIVE' AND NOT is_deleted FOR UPDATE",vec![actor.tenant().into()]).await?;
    let updated=tx.execute(sql("UPDATE kabipay_ops.organization_company SET status='ACTIVE',version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND tenant_id=$3 AND status='PENDING' AND version=$4 AND effective_from<=NOW() AND (effective_to IS NULL OR NOW()<effective_to)",vec![membership.into(),org.into(),actor.tenant().into(),version.into()])).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict());
    }
    Ok((membership, "ACTIVE", version + 1))
}
pub(crate) async fn suspend(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    membership: Uuid,
    version: i64,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(membership)?;
    if version <= 0 || version == i64::MAX {
        return Err(conflict());
    }
    require_surviving_admin(tx, actor, org, membership).await?;
    let updated=tx.execute(sql("UPDATE kabipay_ops.organization_company SET status='SUSPENDED',version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND tenant_id<>$3 AND status='ACTIVE' AND version=$4",vec![membership.into(),org.into(),actor.tenant().into(),version.into()])).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict());
    }
    tx.execute(sql("UPDATE kabipay_ops.role_binding_company SET activation_status='REVOKED',expected_grant_version=expected_grant_version+1 WHERE organization_id=$1 AND organization_company_id=$2 AND activation_status<>'REVOKED'",vec![org.into(),membership.into()])).await?;
    Ok((membership, "SUSPENDED", version + 1))
}

/// The initiating administrator must retain authority through another company
/// so a source ACK is recoverable after the target is paused or retired.
async fn require_surviving_admin(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    target: Uuid,
) -> KabiPayResult<()> {
    let p = current(tx, actor).await?;
    one(tx,"SELECT b.id FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id JOIN kabipay_ops.organization_company m ON m.id=s.organization_company_id AND m.organization_id=b.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE b.organization_id=$1 AND b.principal_id=$2 AND b.status='ACTIVE' AND r.status='ACTIVE' AND c.capability_code='organization:manage' AND c.employee_scope='ALL' AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND m.id<>$3 AND m.status='ACTIVE' AND t.status='ACTIVE' AND NOT t.is_deleted AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) LIMIT 1",vec![org.into(),p.id.into(),target.into()]).await?;
    Ok(())
}
pub(crate) async fn resume(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    membership: Uuid,
    version: i64,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(membership)?;
    if version <= 0 || version == i64::MAX {
        return Err(conflict());
    }
    let changed=tx.execute(sql("UPDATE kabipay_ops.organization_company SET status='ACTIVE',version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND status='SUSPENDED' AND version=$3 AND effective_to IS NULL",vec![membership.into(),org.into(),version.into()])).await?;
    if changed.rows_affected() != 1 {
        return Err(conflict());
    }
    tx.execute(sql("UPDATE kabipay_ops.role_binding_company s SET activation_status='PENDING',expected_grant_version=b.version,installed_hash=NULL FROM kabipay_ops.principal_role_binding b WHERE s.binding_id=b.id AND s.organization_id=b.organization_id AND s.organization_id=$1 AND s.organization_company_id=$2 AND s.activation_status='REVOKED' AND b.status IN ('ACTIVE','PENDING')",vec![org.into(),membership.into()])).await?;
    let _ = actor;
    Ok((membership, "ACTIVE", version + 1))
}
pub(crate) async fn begin_detach(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
    membership: Uuid,
    version: i64,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(membership)?;
    if version <= 0 || version == i64::MAX {
        return Err(conflict());
    }
    require_surviving_admin(tx, actor, org, membership).await?;
    // Other companies must explicitly reassign reports and confirm the source
    // installation before this manager's company can leave.
    let incoming=tx.query_one(sql("SELECT a.id FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id AND manager.organization_id=a.organization_id WHERE a.organization_id=$1 AND manager.organization_company_id=$2 AND a.organization_company_id<>$2 AND (a.status<>'ENDED' OR a.installed_version IS DISTINCT FROM a.version OR a.installed_configuration->>'status' IS DISTINCT FROM 'ENDED') LIMIT 1",vec![org.into(),membership.into()])).await?;
    if incoming.is_some() {
        return Err(kabipay_common::KabiPayError::ConflictRule{code:"ORGANIZATION_REPORTS_REQUIRE_REASSIGNMENT",message:"Reassign or end and install all reports in other companies before detaching this company.".into()});
    }
    let changed=tx.execute(sql("UPDATE kabipay_ops.organization_company SET status='DETACHING',version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND tenant_id<>$3 AND status IN ('ACTIVE','SUSPENDED') AND version=$4",vec![membership.into(),org.into(),actor.tenant().into(),version.into()])).await?;
    if changed.rows_affected() != 1 {
        return Err(conflict());
    }
    tx.execute(sql("UPDATE kabipay_ops.role_binding_company SET activation_status='REVOKED',expected_grant_version=expected_grant_version+1 WHERE organization_id=$1 AND organization_company_id=$2 AND activation_status<>'REVOKED'",vec![org.into(),membership.into()])).await?;
    tx.execute(sql("UPDATE kabipay_ops.organization_relationship SET status='ENDED',version=version+1,updated_at=NOW() WHERE organization_id=$1 AND organization_company_id=$2 AND status<>'ENDED'",vec![org.into(),membership.into()])).await?;
    Ok((membership, "DETACHING", version + 1))
}
