//! Profile review checks no decrypted payload, banking or document value.
use kabipay_common::{context::ScopeType, important_email::*, KabiPayResult};
use kabipay_organization_tenant::{email_routes::EmailDomainAdapter, statement};
use sea_orm::{ConnectionTrait, DatabaseConnection};
#[derive(Clone)]
pub struct EmployeeEmailAdapter;
impl EmailDomainAdapter for EmployeeEmailAdapter {
    fn domain(&self) -> &'static str {
        "employee"
    }
    async fn check_email_eligibility(
        &self,
        db: &DatabaseConnection,
        ops: &DatabaseConnection,
        request: &EmailEligibilityRequest,
    ) -> KabiPayResult<EmailEligibilityDecision> {
        check_email_eligibility(db, ops, request).await
    }
}
pub const EMAIL_RESOURCE_SQL:&str="SELECT employee_id,requested_by,status,updated_at FROM employee_profile_change_request WHERE tenant_id=$1 AND id=$2";
pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    if event.resource_type != EmailResourceType::ProfileChange {
        return Ok(EmailEligibilityDecision::Forbidden);
    }
    let Some(row) = db
        .query_one(statement(
            EMAIL_RESOURCE_SQL,
            vec![event.tenant_id.into(), event.resource_id.into()],
        ))
        .await?
    else {
        return Ok(EmailEligibilityDecision::Obsolete);
    };
    let status: String = row.try_get("", "status")?;
    let subject = EmailSubjectSnapshot {
        employee_id: row.try_get("", "employee_id")?,
        requester_user_id: Some(row.try_get("", "requested_by")?),
        resource_version: email_timestamp_version(&status, row.try_get("", "updated_at")?),
        pending: status == "PENDING",
        outcome_is_current: email_outcome_matches(event, &status.to_ascii_lowercase())
            && event.resource_version == email_timestamp_version(&status,row.try_get("", "updated_at")?),
    };
    match check_email_subject(event, &subject) {
        EmailSubjectCheck::RequesterOutcome => return Ok(EmailEligibilityDecision::Eligible),
        EmailSubjectCheck::Obsolete => return Ok(EmailEligibilityDecision::Obsolete),
        EmailSubjectCheck::CheckCurrentActionAuthority => {}
        _ => return Ok(EmailEligibilityDecision::Forbidden),
    }
    let EmailRecipientRef::LocalUser { user_id } = event.recipient else {
        return Ok(EmailEligibilityDecision::Forbidden);
    };
    if !email_outcome_matches(event, "pending") {
        return Ok(EmailEligibilityDecision::Forbidden);
    }
    // Match can_manage_employee_directory, including its existing compatibility
    // permission. No role label or resource-wide scope fallback is consulted.
    for permission in ["employee:write", "employee:manage"] {
        if current_email_permission_scope(db, event.tenant_id, user_id, permission).await?
            == Some(ScopeType::All)
        {
            return Ok(EmailEligibilityDecision::Eligible);
        }
    }
    Ok(EmailEligibilityDecision::Forbidden)
}
