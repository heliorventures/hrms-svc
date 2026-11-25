//! Shared current metadata identity gate; no mailbox or profile data is read.
use super::{EmailActionTarget, EmailEvent, EmailRecipientRef};
use crate::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;
pub const EMAIL_ORGANIZATION_IDENTITY_SQL: &str = r#"
SELECT e.source_user_id FROM kabipay_ops.employment_ref e
JOIN kabipay_ops.principal p ON p.id=e.principal_id AND p.status='ACTIVE'
JOIN kabipay_ops.principal_login_link l ON l.principal_id=p.id AND l.source_tenant_id=e.source_tenant_id
 AND l.source_user_id=e.source_user_id AND l.issuer='kabipay-client' AND l.status='VERIFIED'
JOIN kabipay_ops.organization_company source ON source.id=e.organization_company_id AND source.organization_id=e.organization_id
JOIN kabipay_ops.organization_company owning ON owning.id=$5 AND owning.organization_id=e.organization_id AND owning.tenant_id=$6
JOIN kabipay_ops.organization o ON o.id=e.organization_id AND o.status='ACTIVE'
JOIN kabipay_ops.tenant st ON st.id=source.tenant_id AND st.status='ACTIVE' AND NOT st.is_deleted
JOIN kabipay_ops.tenant ot ON ot.id=owning.tenant_id AND ot.status='ACTIVE' AND NOT ot.is_deleted
WHERE e.principal_id=$1 AND e.source_tenant_id=$2 AND e.source_employee_id=$3 AND e.organization_id=$4
 AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')
 AND source.status='ACTIVE' AND source.installed_version=source.version AND source.runtime_status='ACTIVE'
 AND source.effective_from<=clock_timestamp() AND (source.effective_to IS NULL OR source.effective_to>clock_timestamp())
 AND owning.status='ACTIVE' AND owning.installed_version=owning.version AND owning.runtime_status='ACTIVE'
 AND owning.effective_from<=clock_timestamp() AND (owning.effective_to IS NULL OR owning.effective_to>clock_timestamp())
LIMIT 2
"#;

pub async fn current_email_organization_user(
    ops: &impl ConnectionTrait,
    event: &EmailEvent,
) -> KabiPayResult<Uuid> {
    let denied = || KabiPayError::Unauthorised;
    let EmailRecipientRef::OrganizationPrincipal {
        principal_id,
        source_tenant_id,
        source_employee_id,
    } = event.recipient
    else {
        return Err(denied());
    };
    let EmailActionTarget::OrganizationResource {
        organization_id,
        membership_id,
        ..
    } = event.action_target
    else {
        return Err(denied());
    };
    let rows = ops
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            EMAIL_ORGANIZATION_IDENTITY_SQL,
            [
                principal_id.into(),
                source_tenant_id.into(),
                source_employee_id.into(),
                organization_id.into(),
                membership_id.into(),
                event.tenant_id.into(),
            ],
        ))
        .await?;
    if rows.len() != 1 {
        return Err(denied());
    }
    Ok(rows[0].try_get("", "source_user_id")?)
}
