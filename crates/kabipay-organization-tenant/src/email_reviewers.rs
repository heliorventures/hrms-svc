//! Bounded installed-authority selection, checked with the explicitly supplied
//! metadata connection. Never discovers a company DSN or reads a mailbox.
use crate::statement;
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub struct OrganizationEmailReviewer {
    pub reviewer: EmailReviewer,
    pub scope: String,
    pub is_reporting_manager: bool,
    pub source_user_id: Uuid,
}
#[derive(Serialize, Deserialize)]
struct Candidate {
    organization_id: Uuid,
    membership_id: Uuid,
    principal_id: Uuid,
    authority_id: Uuid,
    authority_version: i64,
    membership_version: i64,
    relationship: bool,
    scope: String,
    source_is_reporting_manager: bool,
}
pub const EMAIL_INSTALLED_REVIEWERS_SQL: &str = r#"
WITH candidates AS (
 SELECT g.organization_id,g.membership_id,g.principal_id,g.binding_id AS authority_id,
 g.grant_version AS authority_version,m.version AS membership_version,FALSE AS relationship,c.employee_scope AS scope
 FROM organization_grant_installation g JOIN organization_membership_installation m
 ON m.tenant_id=g.tenant_id AND m.organization_id=g.organization_id AND m.membership_id=g.membership_id
 AND m.status='ACTIVE' AND m.version=g.membership_version
 CROSS JOIN LATERAL jsonb_to_recordset(g.capabilities) AS c(code text,employee_scope text)
 WHERE g.tenant_id=$1 AND g.status='ACTIVE' AND c.code=$3 AND c.employee_scope IN ('ALL','DEPARTMENT')
 UNION ALL
 SELECT a.organization_id,a.membership_id,a.manager_principal_id,a.id,a.version,m.version,TRUE,'TEAM'
 FROM management_assignment a JOIN organization_membership_installation m
 ON m.tenant_id=a.tenant_id AND m.organization_id=a.organization_id AND m.membership_id=a.membership_id AND m.status='ACTIVE'
 JOIN assignment_capability c ON c.tenant_id=a.tenant_id AND c.assignment_id=a.id AND c.capability_code=$3
 WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.status='ACTIVE' AND a.subject_principal_id<>a.manager_principal_id
 AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())
)
SELECT c.*,EXISTS(SELECT 1 FROM management_assignment a JOIN assignment_capability p
 ON p.tenant_id=a.tenant_id AND p.assignment_id=a.id AND p.capability_code=$3
 WHERE a.tenant_id=$1 AND a.organization_id=c.organization_id AND a.membership_id=c.membership_id
 AND a.subject_employee_id=$2 AND a.manager_principal_id=c.principal_id AND a.relationship_type='PRIMARY'
 AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())) AS source_is_reporting_manager
FROM candidates c WHERE NOT EXISTS(SELECT 1 FROM management_assignment own
 WHERE own.tenant_id=$1 AND own.subject_employee_id=$2 AND own.subject_principal_id=c.principal_id)
ORDER BY c.organization_id,c.membership_id,c.principal_id,c.relationship,c.authority_id LIMIT 1001
"#;

/// Same current metadata predicates as the private dispatch authority boundary,
/// applied to all installed candidates in one bounded query. Scope is never
/// supplied by an event producer; it must match both the installed and current
/// organization capability. A changed source login cannot reuse a stale ref.
pub const EMAIL_METADATA_REVIEWERS_SQL: &str = r#"
WITH candidates AS (
 SELECT * FROM jsonb_to_recordset($1::jsonb) AS c(organization_id uuid,membership_id uuid,principal_id uuid,
 authority_id uuid,authority_version bigint,membership_version bigint,relationship boolean,scope text,source_is_reporting_manager boolean)
)
SELECT DISTINCT ON(c.organization_id,c.membership_id,c.authority_id,c.principal_id)
 c.organization_id,c.membership_id,c.principal_id,c.authority_id,c.authority_version,c.relationship,c.scope,
 e.source_tenant_id,e.source_employee_id,
 c.source_is_reporting_manager AND EXISTS(SELECT 1 FROM kabipay_ops.organization_relationship primary_rel
 JOIN kabipay_ops.employment_ref me ON me.id=primary_rel.manager_employment_ref_id AND me.organization_id=primary_rel.organization_id
 WHERE primary_rel.organization_id=c.organization_id AND primary_rel.organization_company_id=c.membership_id
 AND primary_rel.subject_employment_ref_id=subject.id AND me.principal_id=c.principal_id
 AND me.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND primary_rel.relationship_type='PRIMARY'
 AND primary_rel.status='ACTIVE' AND primary_rel.installed_version=primary_rel.version
 AND primary_rel.effective_from<=clock_timestamp() AND (primary_rel.effective_to IS NULL OR primary_rel.effective_to>clock_timestamp())
 AND primary_rel.capabilities @> jsonb_build_array($4::text)) AS is_reporting_manager,e.source_user_id
FROM candidates c
JOIN kabipay_ops.organization_company owning ON owning.id=c.membership_id AND owning.organization_id=c.organization_id
 AND owning.tenant_id=$2 AND owning.status='ACTIVE' AND owning.version=c.membership_version
 AND owning.installed_version=owning.version AND owning.runtime_status='ACTIVE'
 AND owning.effective_from<=clock_timestamp() AND (owning.effective_to IS NULL OR owning.effective_to>clock_timestamp())
JOIN kabipay_ops.organization o ON o.id=c.organization_id AND o.status='ACTIVE'
JOIN kabipay_ops.tenant ot ON ot.id=owning.tenant_id AND ot.status='ACTIVE' AND NOT ot.is_deleted
JOIN kabipay_ops.employment_ref subject ON subject.organization_id=c.organization_id AND subject.organization_company_id=c.membership_id
 AND subject.source_tenant_id=$2 AND subject.source_employee_id=$3 AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')
 AND subject.principal_id IS DISTINCT FROM c.principal_id
JOIN kabipay_ops.employment_ref e ON e.organization_id=c.organization_id AND e.principal_id=c.principal_id
 AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')
JOIN kabipay_ops.principal p ON p.id=e.principal_id AND p.status='ACTIVE'
JOIN kabipay_ops.principal_login_link l ON l.principal_id=p.id AND l.source_tenant_id=e.source_tenant_id
 AND l.source_user_id=e.source_user_id AND l.issuer='kabipay-client' AND l.status='VERIFIED'
JOIN kabipay_ops.organization_company source ON source.id=e.organization_company_id AND source.organization_id=e.organization_id
 AND source.tenant_id=e.source_tenant_id AND source.status='ACTIVE' AND source.installed_version=source.version AND source.runtime_status='ACTIVE'
 AND source.effective_from<=clock_timestamp() AND (source.effective_to IS NULL OR source.effective_to>clock_timestamp())
JOIN kabipay_ops.tenant st ON st.id=source.tenant_id AND st.status='ACTIVE' AND NOT st.is_deleted
WHERE CASE WHEN c.relationship THEN EXISTS(
 SELECT 1 FROM kabipay_ops.organization_relationship r
 WHERE r.id=c.authority_id AND r.organization_id=c.organization_id AND r.organization_company_id=c.membership_id
 AND r.manager_employment_ref_id=e.id AND r.subject_employment_ref_id=subject.id AND r.version=c.authority_version
 AND r.installed_version=r.version AND r.status='ACTIVE' AND r.effective_from<=clock_timestamp()
 AND (r.effective_to IS NULL OR r.effective_to>clock_timestamp()) AND r.capabilities @> jsonb_build_array($4::text) AND c.scope='TEAM'
) ELSE EXISTS(
 SELECT 1 FROM kabipay_ops.principal_role_binding b
 JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id AND r.status='ACTIVE'
 JOIN kabipay_ops.organization_role_capability cap ON cap.role_id=r.id AND cap.organization_id=r.organization_id
 AND cap.capability_code=$4 AND cap.employee_scope=c.scope
 JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id
 AND s.organization_company_id=c.membership_id AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version
 WHERE b.id=c.authority_id AND b.organization_id=c.organization_id AND b.principal_id=c.principal_id
 AND b.status='ACTIVE' AND b.version=c.authority_version AND b.effective_from<=clock_timestamp()
 AND (b.effective_to IS NULL OR b.effective_to>clock_timestamp())
 AND (c.scope='ALL' OR (c.scope='DEPARTMENT' AND subject.department_id IS NOT NULL AND EXISTS(
 SELECT 1 FROM kabipay_ops.employment_ref actor WHERE actor.organization_id=c.organization_id
 AND actor.organization_company_id=c.membership_id AND actor.principal_id=c.principal_id
 AND actor.department_id=subject.department_id AND actor.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE'))))
) END
ORDER BY c.organization_id,c.membership_id,c.authority_id,c.principal_id,(e.source_tenant_id<>$2),e.source_tenant_id,e.source_employee_id
LIMIT 1001
"#;

pub async fn select_organization_email_reviewers(
    db: &impl ConnectionTrait,
    ops: &impl ConnectionTrait,
    tenant: Uuid,
    subject: Uuid,
    resource: EmailResourceType,
    id: Uuid,
) -> KabiPayResult<Vec<OrganizationEmailReviewer>> {
    if !matches!(
        resource,
        EmailResourceType::Leave
            | EmailResourceType::Timesheet
            | EmailResourceType::PerformanceReview
    ) {
        return Ok(vec![]);
    }
    if tenant.is_nil() || subject.is_nil() || id.is_nil() {
        return Err(KabiPayError::Unauthorised);
    }
    let permission = email_action_permission(resource).ok_or(KabiPayError::Unauthorised)?;
    let rows = db
        .query_all(statement(
            EMAIL_INSTALLED_REVIEWERS_SQL,
            vec![tenant.into(), subject.into(), permission.into()],
        ))
        .await?;
    if rows.len() > MAX_EMAIL_REVIEWERS {
        return Err(capacity());
    }
    let mut candidates = Vec::with_capacity(rows.len());
    for r in rows {
        candidates.push(Candidate {
            organization_id: r.try_get("", "organization_id")?,
            membership_id: r.try_get("", "membership_id")?,
            principal_id: r.try_get("", "principal_id")?,
            authority_id: r.try_get("", "authority_id")?,
            authority_version: r.try_get("", "authority_version")?,
            membership_version: r.try_get("", "membership_version")?,
            relationship: r.try_get("", "relationship")?,
            scope: r.try_get("", "scope")?,
            source_is_reporting_manager: r.try_get("", "source_is_reporting_manager")?,
        });
    }
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    let rows = ops
        .query_all(statement(
            EMAIL_METADATA_REVIEWERS_SQL,
            vec![
                Value::from(serde_json::to_value(candidates)?),
                tenant.into(),
                subject.into(),
                permission.into(),
            ],
        ))
        .await?;
    if rows.len() > MAX_EMAIL_REVIEWERS {
        return Err(capacity());
    }
    let mut result = Vec::with_capacity(rows.len());
    for r in rows {
        let organization_id = r.try_get("", "organization_id")?;
        let membership_id = r.try_get("", "membership_id")?;
        let authority_id = r.try_get("", "authority_id")?;
        let version = r.try_get("", "authority_version")?;
        let recipient = EmailRecipientRef::OrganizationPrincipal {
            principal_id: r.try_get("", "principal_id")?,
            source_tenant_id: r.try_get("", "source_tenant_id")?,
            source_employee_id: r.try_get("", "source_employee_id")?,
        };
        if !recipient.valid()
            || organization_id == Uuid::nil()
            || membership_id == Uuid::nil()
            || authority_id == Uuid::nil()
            || version <= 0
        {
            return Err(KabiPayError::Unauthorised);
        }
        let authorization_ref = if r.try_get::<bool>("", "relationship")? {
            EmailAuthorizationRef::OrganizationRelationship {
                organization_id,
                membership_id,
                relationship_id: authority_id,
                version,
            }
        } else {
            EmailAuthorizationRef::OrganizationGrant {
                organization_id,
                membership_id,
                binding_id: authority_id,
                version,
            }
        };
        result.push(OrganizationEmailReviewer {
            reviewer: EmailReviewer {
                recipient,
                authorization_ref,
                action_target: EmailActionTarget::OrganizationResource {
                    organization_id,
                    membership_id,
                    resource_type: resource,
                    resource_id: id,
                },
            },
            scope: r.try_get("", "scope")?,
            is_reporting_manager: r.try_get("", "is_reporting_manager")?,
            source_user_id: r.try_get("", "source_user_id")?,
        });
    }
    Ok(result)
}
fn capacity() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "EMAIL_REVIEWER_CAPACITY_EXCEEDED",
        message: "important alert reviewer selection exceeds its bounded capacity".into(),
    }
}
