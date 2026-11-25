//! Existing Expense/Travel workflow authority; private metadata only.
use kabipay_common::{important_email::*, KabiPayResult};
use kabipay_organization_tenant::{
    email_routes::EmailDomainAdapter,
    email_workflow::{check_workflow_email, WorkflowEmailResource},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
#[derive(Clone)]
pub struct ExpenseEmailAdapter;
impl EmailDomainAdapter for ExpenseEmailAdapter {
    fn domain(&self) -> &'static str {
        "expense"
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
pub const EMAIL_EXPENSE_SQL:&str="SELECT r.employee_id,r.status,r.updated_at,r.workflow_instance_id,e.user_id AS requester_user_id,e.status AS employee_status FROM expense r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id AND NOT e.is_deleted WHERE r.tenant_id=$1 AND r.id=$2 AND NOT r.is_deleted";
pub const EMAIL_TRAVEL_SQL:&str="SELECT r.employee_id,r.status,r.updated_at,r.workflow_instance_id,e.user_id AS requester_user_id,e.status AS employee_status FROM travel_request r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id AND NOT e.is_deleted WHERE r.tenant_id=$1 AND r.id=$2";
pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    let (query, entity) = match event.resource_type {
        EmailResourceType::Expense => (
            EMAIL_EXPENSE_SQL,
            crate::services::expense_service::WF_ENTITY_EXPENSE,
        ),
        EmailResourceType::Travel => (
            EMAIL_TRAVEL_SQL,
            crate::services::travel_request_service::WF_ENTITY_TRAVEL_REQUEST,
        ),
        _ => return Ok(EmailEligibilityDecision::Forbidden),
    };
    let Some(row) = db
        .query_one(statement(
            query,
            vec![event.tenant_id.into(), event.resource_id.into()],
        ))
        .await?
    else {
        return Ok(EmailEligibilityDecision::Obsolete);
    };
    let status: String = row.try_get("", "status")?;
    let active: String = row.try_get("", "employee_status")?;
    let subject = EmailSubjectSnapshot {
        employee_id: row.try_get("", "employee_id")?,
        requester_user_id: row.try_get("", "requester_user_id")?,
        resource_version: email_timestamp_version(&status, row.try_get("", "updated_at")?),
        pending: status == "PENDING"
            && matches!(active.as_str(), "ACTIVE" | "PROBATION" | "ON_LEAVE"),
        outcome_is_current: workflow_email_outcome_is_current(
            db,
            event,
            row.try_get("", "workflow_instance_id")?,
            entity,
            &status,
            row.try_get("", "updated_at")?,
        )
        .await?,
    };
    check_workflow_email(
        db,
        request,
        subject,
        WorkflowEmailResource {
            instance_id: row.try_get("", "workflow_instance_id")?,
            entity_type: entity,
            require_active_definition: false,
        },
    )
    .await
}
