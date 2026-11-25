//! Tax-proof review uses the same TEAM/ALL target and self-review rules.
use kabipay_common::{
    client_data_scope::resolve_employee_scope_filter_with_connection, context::ScopeType,
    important_email::*, KabiPayResult,
};
use kabipay_organization_tenant::{
    email_routes::EmailDomainAdapter, email_workflow::action_decision, statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
#[derive(Clone)]
pub struct TaxEmailAdapter;
impl EmailDomainAdapter for TaxEmailAdapter {
    fn domain(&self) -> &'static str {
        "tax"
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
pub const EMAIL_RESOURCE_SQL:&str="SELECT r.employee_id,r.status,r.updated_at,e.user_id AS requester_user_id FROM tax_proof_line r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id AND NOT e.is_deleted WHERE r.tenant_id=$1 AND r.id=$2";
pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    if event.resource_type != EmailResourceType::TaxProof {
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
        requester_user_id: row.try_get("", "requester_user_id")?,
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
    let Some(scope) =
        current_email_permission_scope(db, event.tenant_id, user_id, "tax:approve").await?
    else {
        return Ok(EmailEligibilityDecision::Forbidden);
    };
    if ![ScopeType::Team, ScopeType::All].contains(&scope) {
        return Ok(EmailEligibilityDecision::Forbidden);
    }
    let Some(actor) = current_email_actor(db, event.tenant_id, user_id).await? else {
        return Ok(EmailEligibilityDecision::Forbidden);
    };
    let filter =
        resolve_employee_scope_filter_with_connection(db, event.tenant_id, scope, Some(actor))
            .await?;
    action_decision(crate::services::tax_service::require_tax_approval_target(
        &filter,
        actor.employee_id,
        subject.employee_id,
    ))
}
