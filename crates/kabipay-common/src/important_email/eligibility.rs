//! Fresh mail authority uses the same exact permission/scope merge as login.
//! No role names, JWT snapshots, cached authorization or address reads.
use super::EmailResourceType;
use crate::{
    context::{ClientViewerEmployee, ScopeType},
    KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

pub const CURRENT_EMAIL_PERMISSION_SQL: &str = r#"
SELECT ps.scope_type FROM "user" u
JOIN user_role ur ON ur.user_id=u.id
JOIN role r ON r.id=ur.role_id AND r.tenant_id=u.tenant_id AND NOT r.is_deleted
JOIN role_permission rp ON rp.role_id=r.id
JOIN permission p ON p.id=rp.permission_id
JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=r.tenant_id
 AND lower(ps.resource)=lower(p.resource) AND lower(ps.action)=lower(p.action)
WHERE u.tenant_id=$1 AND u.id=$2 AND u.is_active AND NOT u.is_deleted
 AND lower(p.resource)=$3 AND lower(p.action)=$4
"#;

pub async fn current_email_permission_scope(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    user: Uuid,
    permission: &str,
) -> KabiPayResult<Option<ScopeType>> {
    let parts: Vec<_> = permission.split(':').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 64
                || !part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        || tenant.is_nil()
        || user.is_nil()
    {
        return Err(KabiPayError::Unauthorised);
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            CURRENT_EMAIL_PERMISSION_SQL,
            [tenant.into(), user.into(), parts[0].into(), parts[1].into()],
        ))
        .await?;
    let mut best = None;
    for row in rows {
        if let Some(scope) = ScopeType::parse_loose(&row.try_get::<String>("", "scope_type")?) {
            if best.is_none_or(|old: ScopeType| scope.rank() > old.rank()) {
                best = Some(scope);
            }
        }
    }
    Ok(best)
}

/// Ambiguous, inactive, deleted and unlinked employment never receives an
/// actionable alert. Selection queries use the same bounded identity rule.
pub async fn current_email_actor(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    user: Uuid,
) -> KabiPayResult<Option<ClientViewerEmployee>> {
    let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT e.id,e.department_id FROM employee e JOIN \"user\" u ON u.id=e.user_id AND u.tenant_id=e.tenant_id WHERE e.tenant_id=$1 AND e.user_id=$2 AND NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND u.is_active AND NOT u.is_deleted LIMIT 2",
        [tenant.into(), user.into()])).await?;
    if rows.len() != 1 {
        return Ok(None);
    }
    Ok(Some(ClientViewerEmployee {
        employee_id: rows[0].try_get("", "id")?,
        department_id: rows[0].try_get("", "department_id")?,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailEligibilityDecision {
    Eligible,
    Obsolete,
    Forbidden,
}

pub struct EmailSubjectSnapshot {
    pub employee_id: Uuid,
    pub requester_user_id: Option<Uuid>,
    pub resource_version: String,
    pub pending: bool,
    /// Owning domain confirms the outcome was not corrected/reversed. This is
    /// independent of pending status and unrelated later timestamp changes.
    pub outcome_is_current: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailSubjectCheck {
    RequesterOutcome,
    CheckCurrentActionAuthority,
    Obsolete,
    Forbidden,
}

/// This is a subject/transition gate, NOT a grant of action authority. A domain
/// must prove persistence first and must run its current action policy after
/// CheckCurrentActionAuthority. A committed outcome remains historical truth
/// when the resource advances; an old action request does not.
pub fn check_email_subject(
    event: &super::EmailEvent,
    subject: &EmailSubjectSnapshot,
) -> EmailSubjectCheck {
    use super::{EmailEventKind, EmailRecipientRef};
    let requester = match event.recipient {
        EmailRecipientRef::LocalUser { user_id } => subject.requester_user_id == Some(user_id),
        EmailRecipientRef::OrganizationPrincipal {
            source_tenant_id,
            source_employee_id,
            ..
        } => source_tenant_id == event.tenant_id && source_employee_id == subject.employee_id,
    };
    match event.kind {
        EmailEventKind::Outcome => {
            if !subject.outcome_is_current {
                EmailSubjectCheck::Obsolete
            } else if requester {
                EmailSubjectCheck::RequesterOutcome
            } else {
                EmailSubjectCheck::Forbidden
            }
        }
        EmailEventKind::ActionRequired => {
            if !subject.pending || event.resource_version != subject.resource_version {
                EmailSubjectCheck::Obsolete
            } else if requester {
                EmailSubjectCheck::Forbidden
            } else {
                EmailSubjectCheck::CheckCurrentActionAuthority
            }
        }
        _ => EmailSubjectCheck::Forbidden,
    }
}

pub fn email_outcome_matches(event: &super::EmailEvent, transition: &str) -> bool {
    matches!(&event.authorization_ref, super::EmailAuthorizationRef::DomainTransition { resource_type, transition_key }
        if *resource_type == event.resource_type && transition_key == transition)
}

pub fn email_timestamp_version(status: &str, at: chrono::DateTime<chrono::Utc>) -> String {
    format!("{}:{}", status, at.timestamp_micros())
}

/// Intermediate approvals may update only workflow_instance. Bind both the
/// domain revision and exact step/revision instead of treating the unchanged
/// request.updated_at as fresh authority for the following step.
pub fn email_workflow_version(
    base: &str,
    instance: Uuid,
    step: Uuid,
    at: chrono::DateTime<chrono::Utc>,
) -> String {
    use sha2::Digest;
    format!(
        "wf:{:x}:{instance}:{step}:{}",
        sha2::Sha256::digest(base.as_bytes()),
        at.timestamp_micros()
    )
}

pub fn email_action_permission(resource: EmailResourceType) -> Option<&'static str> {
    use EmailResourceType::*;
    Some(match resource {
        Leave => "leave:approve",
        Expense => "expense:approve",
        Travel => "travel:approve",
        TaxProof => "tax:approve",
        Timesheet => "timesheet:approve",
        ProfileChange => "employee:write",
        LoanRequest => "loan:approve",
        PerformanceReview => "performance:evaluate",
        _ => return None,
    })
}
