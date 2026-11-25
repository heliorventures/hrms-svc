//! Bounded current reviewer identities. No mailbox or secret projection.
use crate::{context::ScopeType, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::HashSet;
use uuid::Uuid;

pub const MAX_EMAIL_REVIEWERS: usize = 1000;
pub struct LocalEmailReviewer {
    pub user_id: Uuid,
    pub employee_id: Uuid,
    pub scope: ScopeType,
    pub is_reporting_manager: bool,
}

/// Reverse the existing canonical TEAM relationship for one subject instead of
/// issuing a scope query for every role holder. A foreign PRIMARY deliberately
/// suppresses the legacy local manager, exactly as the normal business filter.
/// Permission ranking matches ScopeType::rank; only the exact same-role grant
/// and scope pair participates. Ambiguous employment links are never selected.
pub const CURRENT_REVIEWER_BATCH_SQL: &str = r#"
WITH RECURSIVE subject AS (
 SELECT e.id,e.user_id,e.department_id,e.status,
 CASE WHEN a.id IS NOT NULL THEN a.manager_employee_id ELSE e.reporting_manager_id END AS manager_id
 FROM employee e LEFT JOIN management_assignment a ON a.tenant_id=e.tenant_id AND a.subject_employee_id=e.id
 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp()
 AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())
 WHERE e.tenant_id=$1 AND e.id=$2 AND NOT e.is_deleted
), ancestors(id,path,next_id,depth) AS (
 SELECT id,ARRAY[id],manager_id,1 FROM subject WHERE status IN ('ACTIVE','PROBATION')
 UNION ALL
 SELECT e.id,a.path||e.id,
 CASE WHEN m.id IS NOT NULL THEN m.manager_employee_id ELSE e.reporting_manager_id END,a.depth+1
 FROM ancestors a JOIN employee e ON e.tenant_id=$1 AND e.id=a.next_id
 LEFT JOIN management_assignment m ON m.tenant_id=e.tenant_id AND m.subject_employee_id=e.id
 AND m.relationship_type='PRIMARY' AND m.status='ACTIVE' AND m.effective_from<=clock_timestamp()
 AND (m.effective_to IS NULL OR m.effective_to>clock_timestamp())
 WHERE NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION') AND NOT e.id=ANY(a.path) AND a.depth<=512
), permissions AS (
 SELECT u.id AS user_id,MAX(CASE upper(trim(ps.scope_type)) WHEN 'ALL' THEN 4
 WHEN 'DEPARTMENT' THEN 3 WHEN 'TEAM' THEN 2 WHEN 'SELF' THEN 1 ELSE 0 END)::integer AS scope_rank
 FROM "user" u JOIN user_role ur ON ur.user_id=u.id
 JOIN role r ON r.id=ur.role_id AND r.tenant_id=u.tenant_id AND NOT r.is_deleted
 JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id
 JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=r.tenant_id
 AND lower(ps.resource)=lower(p.resource) AND lower(ps.action)=lower(p.action)
 WHERE u.tenant_id=$1 AND u.is_active AND NOT u.is_deleted AND lower(p.resource)=$3 AND lower(p.action)=$4
 GROUP BY u.id
)
SELECT p.user_id,e.id AS employee_id,p.scope_rank,COALESCE(s.manager_id=e.id,FALSE) AS is_reporting_manager,
 EXISTS(SELECT 1 FROM ancestors WHERE depth=513 AND next_id IS NOT NULL AND NOT next_id=ANY(path)) AS ancestry_overflow
FROM permissions p JOIN employee e ON e.tenant_id=$1 AND e.user_id=p.user_id CROSS JOIN subject s
WHERE NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND e.id<>s.id
 AND p.user_id IS DISTINCT FROM s.user_id
 AND NOT EXISTS(SELECT 1 FROM employee other WHERE other.tenant_id=e.tenant_id AND other.user_id=e.user_id
 AND other.id<>e.id AND NOT other.is_deleted AND other.status IN ('ACTIVE','PROBATION','ON_LEAVE'))
 AND CASE p.scope_rank WHEN 4 THEN TRUE WHEN 3 THEN e.department_id=s.department_id
 WHEN 2 THEN EXISTS(SELECT 1 FROM ancestors WHERE id=e.id) ELSE FALSE END
ORDER BY p.user_id LIMIT 1001
"#;

pub async fn select_local_email_reviewers(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    subject: Uuid,
    permission: &str,
) -> KabiPayResult<Vec<LocalEmailReviewer>> {
    let parts: Vec<_> = permission.split(':').collect();
    if tenant.is_nil()
        || subject.is_nil()
        || parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 64
                || !part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
    {
        return Err(KabiPayError::Unauthorised);
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            CURRENT_REVIEWER_BATCH_SQL,
            [
                tenant.into(),
                subject.into(),
                parts[0].into(),
                parts[1].into(),
            ],
        ))
        .await?;
    if rows.len() > MAX_EMAIL_REVIEWERS {
        return Err(capacity());
    }
    let mut users = HashSet::new();
    let mut reviewers = Vec::with_capacity(rows.len());
    for row in rows {
        if row.try_get::<bool>("", "ancestry_overflow")? {
            return Err(capacity());
        }
        let user_id: Uuid = row.try_get("", "user_id")?;
        let employee_id: Uuid = row.try_get("", "employee_id")?;
        if user_id.is_nil() || employee_id.is_nil() || !users.insert(user_id) {
            return Err(KabiPayError::Unauthorised);
        }
        let scope = match row.try_get::<i32>("", "scope_rank")? {
            4 => ScopeType::All,
            3 => ScopeType::Department,
            2 => ScopeType::Team,
            _ => return Err(KabiPayError::Unauthorised),
        };
        reviewers.push(LocalEmailReviewer {
            user_id,
            employee_id,
            scope,
            is_reporting_manager: row.try_get("", "is_reporting_manager")?,
        });
    }
    Ok(reviewers)
}
fn capacity() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "EMAIL_REVIEWER_CAPACITY_EXCEEDED",
        message: "important alert reviewer selection exceeds its bounded capacity".into(),
    }
}

/// Account-authorized ALL-scope directory/program reviewers need not have an
/// employee record. Call only for a domain action whose existing rule permits
/// that account authority (Profile review or Performance management).
pub async fn select_all_scope_email_users(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    subject: Uuid,
    permission: &str,
) -> KabiPayResult<Vec<Uuid>> {
    let parts: Vec<_> = permission.split(':').collect();
    if tenant.is_nil()
        || subject.is_nil()
        || parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty() || p.len() > 64 || !p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
    {
        return Err(KabiPayError::Unauthorised);
    }
    let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,r#"
 SELECT DISTINCT u.id AS user_id FROM "user" u JOIN user_role ur ON ur.user_id=u.id
 JOIN role r ON r.id=ur.role_id AND r.tenant_id=u.tenant_id AND NOT r.is_deleted
 JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id
 JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=r.tenant_id
 AND lower(ps.resource)=lower(p.resource) AND lower(ps.action)=lower(p.action)
 WHERE u.tenant_id=$1 AND u.is_active AND NOT u.is_deleted AND lower(p.resource)=$3 AND lower(p.action)=$4
 AND upper(trim(ps.scope_type))='ALL'
 AND u.id IS DISTINCT FROM (SELECT e.user_id FROM employee e WHERE e.tenant_id=$1 AND e.id=$2)
 ORDER BY u.id LIMIT 1001
 "#,[tenant.into(),subject.into(),parts[0].into(),parts[1].into()])).await?;
    if rows.len() > MAX_EMAIL_REVIEWERS {
        return Err(capacity());
    }
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        let user: Uuid = row.try_get("", "user_id")?;
        if user.is_nil() || users.contains(&user) {
            return Err(KabiPayError::Unauthorised);
        }
        users.push(user);
    }
    Ok(users)
}
