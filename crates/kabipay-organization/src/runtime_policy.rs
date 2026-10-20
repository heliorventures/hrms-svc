//! Live authorization is repeated by every source before a company transaction.
use crate::{identity::SourceIdentity, store::*, workspace::source_member};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::*;
use kabipay_organization_security::{command_body_hash, AuthorityKind};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde_json::Value;
use uuid::Uuid;
pub(crate) struct Authority {
    pub kind: AuthorityKind,
    pub id: Uuid,
    pub version: i64,
    pub scope: String,
}
pub(crate) async fn lifecycle_membership(
    tx: &DatabaseTransaction,
    org: Uuid,
    id: Uuid,
) -> KabiPayResult<(Uuid, i64)> {
    let r=one(tx,"SELECT m.tenant_id,m.version FROM kabipay_ops.organization_company m JOIN kabipay_ops.organization o ON o.id=m.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE m.id=$1 AND m.organization_id=$2 AND m.status IN ('ACTIVE','SUSPENDED','DETACHING','DETACHED') AND o.status='ACTIVE' AND t.status='ACTIVE' AND NOT t.is_deleted",vec![id.into(),org.into()]).await?;
    Ok((r.try_get("", "tenant_id")?, r.try_get("", "version")?))
}
pub(crate) async fn membership(
    tx: &DatabaseTransaction,
    org: Uuid,
    id: Uuid,
) -> KabiPayResult<(Uuid, i64)> {
    let r=one(tx,"SELECT m.tenant_id,m.version FROM kabipay_ops.organization_company m JOIN kabipay_ops.organization o ON o.id=m.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE m.id=$1 AND m.organization_id=$2 AND m.status='ACTIVE' AND m.installed_version=m.version AND m.runtime_status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND o.status='ACTIVE' AND t.status='ACTIVE' AND NOT t.is_deleted",vec![id.into(),org.into()]).await?;
    Ok((r.try_get("", "tenant_id")?, r.try_get("", "version")?))
}
pub(crate) async fn resolve_authority(
    tx: &DatabaseTransaction,
    p: Uuid,
    org: Uuid,
    member: Uuid,
    cap: &str,
    employee: Option<Uuid>,
    all_only: bool,
) -> KabiPayResult<Authority> {
    membership(tx, org, member).await?;
    let rows=tx.query_all(sql("SELECT b.id,b.version,c.employee_scope FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id WHERE b.organization_id=$1 AND b.principal_id=$2 AND b.status='ACTIVE' AND r.status='ACTIVE' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND c.capability_code=$3 AND s.organization_company_id=$4 AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version ORDER BY b.id",vec![org.into(),p.into(),cap.into(),member.into()])).await?;
    for r in rows {
        let scope: String = r.try_get("", "employee_scope")?;
        let allowed = match scope.as_str() {
            "ALL" => true,
            "SELF" if !all_only => {
                if let Some(e) = employee {
                    one(tx,"SELECT id FROM kabipay_ops.employment_ref WHERE organization_id=$1 AND organization_company_id=$2 AND source_employee_id=$3 AND principal_id=$4 AND employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![org.into(),member.into(),e.into(),p.into()]).await.is_ok()
                } else {
                    false
                }
            }
            "DEPARTMENT" if !all_only => {
                if let Some(e) = employee {
                    one(tx,"SELECT subject.id FROM kabipay_ops.employment_ref subject JOIN kabipay_ops.employment_ref actor ON actor.organization_id=subject.organization_id AND actor.organization_company_id=subject.organization_company_id AND actor.department_id=subject.department_id WHERE subject.organization_id=$1 AND subject.organization_company_id=$2 AND subject.source_employee_id=$3 AND actor.principal_id=$4 AND subject.department_id IS NOT NULL AND actor.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![org.into(),member.into(),e.into(),p.into()]).await.is_ok()
                } else {
                    false
                }
            }
            _ => false,
        };
        if allowed {
            return Ok(Authority {
                kind: AuthorityKind::OrganizationGrant,
                id: r.try_get("", "id")?,
                version: r.try_get("", "version")?,
                scope,
            });
        }
    }
    if all_only {
        return Err(denied());
    }
    let employee = employee.ok_or_else(denied)?;
    let r=one(tx,"SELECT a.id,a.version FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id AND manager.organization_id=a.organization_id JOIN kabipay_ops.employment_ref subject ON subject.id=a.subject_employment_ref_id AND subject.organization_id=a.organization_id JOIN kabipay_ops.principal mp ON mp.id=manager.principal_id JOIN kabipay_ops.principal sp ON sp.id=subject.principal_id JOIN kabipay_ops.organization_company mm ON mm.id=manager.organization_company_id JOIN kabipay_ops.tenant mt ON mt.id=mm.tenant_id WHERE a.organization_id=$1 AND a.organization_company_id=$2 AND subject.source_employee_id=$3 AND manager.principal_id=$4 AND a.status='ACTIVE' AND a.installed_version=a.version AND a.effective_from<=NOW() AND (a.effective_to IS NULL OR NOW()<a.effective_to) AND a.capabilities @> $5::jsonb AND manager.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND mp.status='ACTIVE' AND sp.status='ACTIVE' AND mm.status='ACTIVE' AND mm.installed_version=mm.version AND mm.runtime_status='ACTIVE' AND mm.effective_from<=NOW() AND (mm.effective_to IS NULL OR NOW()<mm.effective_to) AND mt.status='ACTIVE' AND NOT mt.is_deleted ORDER BY a.id LIMIT 1",vec![org.into(),member.into(),employee.into(),p.into(),serde_json::json!([cap]).into()]).await?;
    Ok(Authority {
        kind: AuthorityKind::ManagementAssignment,
        id: r.try_get("", "id")?,
        version: r.try_get("", "version")?,
        scope: "TEAM".into(),
    })
}
pub(crate) async fn central_authority(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    p: Uuid,
    org: Uuid,
    cap: &str,
) -> KabiPayResult<Authority> {
    authorized(tx, actor, p, org, cap).await?;
    let r=one(tx,"SELECT b.id,b.version FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id JOIN kabipay_ops.organization_company m ON m.id=s.organization_company_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE b.organization_id=$1 AND b.principal_id=$2 AND b.status='ACTIVE' AND r.status='ACTIVE' AND c.capability_code=$3 AND c.employee_scope='ALL' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND t.status='ACTIVE' AND NOT t.is_deleted AND m.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) ORDER BY b.id LIMIT 1",vec![org.into(),p.into(),cap.into()]).await?;
    Ok(Authority {
        kind: AuthorityKind::OrganizationGrant,
        id: r.try_get("", "id")?,
        version: r.try_get("", "version")?,
        scope: "ALL".into(),
    })
}
pub(crate) async fn actor_ref(tx: &DatabaseTransaction, p: Uuid) -> KabiPayResult<ActorReference> {
    let r=one(tx,"SELECT display_name,identity_version FROM kabipay_ops.principal WHERE id=$1 AND status='ACTIVE'",vec![p.into()]).await?;
    Ok(ActorReference {
        principal_id: p,
        display_name: r.try_get("", "display_name")?,
        identity_version: r.try_get("", "identity_version")?,
    })
}
pub(crate) async fn grant_configuration(
    tx: &DatabaseTransaction,
    org: Uuid,
    member: Uuid,
    id: Uuid,
) -> KabiPayResult<GrantInstallation> {
    let (tenant, mv) = membership(tx, org, member).await?;
    let r=one(tx,"SELECT b.principal_id,b.version FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id WHERE b.id=$1 AND b.organization_id=$2 AND b.status IN ('PENDING','ACTIVE') AND r.status='ACTIVE' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND s.organization_company_id=$3 AND s.activation_status IN ('PENDING','ACTIVE') AND s.expected_grant_version=b.version",vec![id.into(),org.into(),member.into()]).await?;
    let principal: Uuid = r.try_get("", "principal_id")?;
    let rows=tx.query_all(sql("SELECT c.capability_code,c.employee_scope FROM kabipay_ops.organization_role_capability c JOIN kabipay_ops.principal_role_binding b ON b.role_id=c.role_id AND b.organization_id=c.organization_id WHERE b.id=$1 ORDER BY c.capability_code",vec![id.into()])).await?;
    let caps = rows
        .iter()
        .map(|r| {
            Ok(InstalledCapability {
                code: r.try_get("", "capability_code")?,
                employee_scope: r.try_get("", "employee_scope")?,
            })
        })
        .collect::<KabiPayResult<Vec<_>>>()?;
    if caps.is_empty() {
        return Err(denied());
    }
    let mut g = GrantInstallation {
        organization_id: org,
        membership_id: member,
        tenant_id: tenant,
        binding_id: id,
        principal_id: principal,
        grant_version: r.try_get("", "version")?,
        membership_version: mv,
        capabilities: caps,
        actor: actor_ref(tx, principal).await?,
        configuration_hash: String::new(),
    };
    g.configuration_hash = command_body_hash(&serde_json::to_vec(&g)?);
    Ok(g)
}
pub(crate) async fn relationship_configuration(
    tx: &DatabaseTransaction,
    org: Uuid,
    member: Uuid,
    id: Uuid,
) -> KabiPayResult<RelationshipInstallation> {
    let (tenant, _) = membership(tx, org, member).await?;
    let r=one(tx,"SELECT a.*,s.source_employee_id,s.principal_id AS subject_principal,m.principal_id AS manager_principal,m.source_tenant_id AS manager_tenant,m.source_employee_id AS manager_employee FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref s ON s.id=a.subject_employment_ref_id AND s.organization_id=a.organization_id JOIN kabipay_ops.employment_ref m ON m.id=a.manager_employment_ref_id AND m.organization_id=a.organization_id JOIN kabipay_ops.organization_company mm ON mm.id=m.organization_company_id WHERE a.id=$1 AND a.organization_id=$2 AND a.organization_company_id=$3 AND (a.status='ENDED' OR (mm.status='ACTIVE' AND s.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND m.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')))",vec![id.into(),org.into(),member.into()]).await?;
    let sp: Uuid = r.try_get("", "subject_principal")?;
    let mp: Uuid = r.try_get("", "manager_principal")?;
    if sp == mp {
        return Err(denied());
    }
    let caps: Value = r.try_get("", "capabilities")?;
    let ended = r.try_get::<String>("", "status")? == "ENDED";
    let mut g = RelationshipInstallation {
        id,
        organization_id: org,
        membership_id: member,
        tenant_id: tenant,
        subject_employee_id: r.try_get("", "source_employee_id")?,
        subject_principal_id: sp,
        manager_principal_id: mp,
        subject_employment_ref_id: r.try_get("", "subject_employment_ref_id")?,
        manager_employment_ref_id: r.try_get("", "manager_employment_ref_id")?,
        manager_tenant_id: r.try_get("", "manager_tenant")?,
        manager_employee_id: if r.try_get::<Uuid>("", "manager_tenant")? == tenant {
            Some(r.try_get("", "manager_employee")?)
        } else {
            None
        },
        relationship_type: r.try_get("", "relationship_type")?,
        status: if r.try_get::<String>("", "status")? == "ENDED" {
            "ENDED".into()
        } else {
            "ACTIVE".into()
        },
        effective_from: r.try_get("", "effective_from")?,
        effective_to: r.try_get("", "effective_to")?,
        version: r.try_get("", "version")?,
        graph_version: r.try_get("", "graph_version")?,
        capabilities: serde_json::from_value(caps)?,
        actors: vec![
            relationship_actor_ref(tx, sp, ended).await?,
            relationship_actor_ref(tx, mp, ended).await?,
        ],
        configuration_hash: String::new(),
    };
    g.configuration_hash = command_body_hash(&serde_json::to_vec(&g)?);
    Ok(g)
}
pub(crate) async fn authority(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    p: Uuid,
    org: Uuid,
    member: Uuid,
    operation: OrganizationOperation,
    employee: Option<Uuid>,
) -> KabiPayResult<Authority> {
    source_member(tx, actor, org).await?;
    if matches!(
        operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    ) {
        return central_authority(tx, actor, p, org, operation.capability()).await;
    }
    if let Some(employee) = employee {
        let row=one(tx,"SELECT principal_id FROM kabipay_ops.employment_ref WHERE organization_id=$1 AND organization_company_id=$2 AND source_employee_id=$3 AND employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![org.into(),member.into(),employee.into()]).await?;
        if row.try_get::<Option<Uuid>>("", "principal_id")? == Some(p) {
            return Err(KabiPayError::Forbidden(
                "self approval, review or feedback is not permitted".into(),
            ));
        }
    }
    let all = operation.is_company_administration()
        || matches!(
            operation,
            OrganizationOperation::PayrollList
                | OrganizationOperation::PayrollDetail
                | OrganizationOperation::PayrollDraft
                | OrganizationOperation::PayrollFinalize
                | OrganizationOperation::PayrollJobList
                | OrganizationOperation::PayrollJobDetail
                | OrganizationOperation::PayrollJobCancelBeforeExecution
        );
    resolve_authority(tx, p, org, member, operation.capability(), employee, all).await
}

async fn relationship_actor_ref(
    tx: &DatabaseTransaction,
    p: Uuid,
    ended: bool,
) -> KabiPayResult<ActorReference> {
    if !ended {
        return actor_ref(tx, p).await;
    }
    let r = one(
        tx,
        "SELECT display_name,identity_version FROM kabipay_ops.principal WHERE id=$1",
        vec![p.into()],
    )
    .await?;
    Ok(ActorReference {
        principal_id: p,
        display_name: r.try_get("", "display_name")?,
        identity_version: r.try_get("", "identity_version")?,
    })
}
