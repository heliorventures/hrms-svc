//! Loan approval and finalized-payslip eligibility, without amounts or exports.
use kabipay_common::{important_email::*, KabiPayResult};
use kabipay_organization_tenant::{
    email_routes::EmailDomainAdapter,
    email_workflow::{check_workflow_email, WorkflowEmailResource},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
#[derive(Clone)]
pub struct PayrollEmailAdapter;
impl EmailDomainAdapter for PayrollEmailAdapter {
    fn domain(&self) -> &'static str {
        "payroll"
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
pub const EMAIL_LOAN_SQL:&str="SELECT r.employee_id,r.state,r.version,r.workflow_instance_id,i.workflow_id,e.user_id AS requester_user_id,e.status AS employee_status FROM loan_request r LEFT JOIN workflow_instance i ON i.tenant_id=r.tenant_id AND i.id=r.workflow_instance_id JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id AND NOT e.is_deleted WHERE r.tenant_id=$1 AND r.id=$2";
pub const EMAIL_PAYSLIP_SQL:&str="SELECT p.status,p.updated_at,c.status AS cycle_status,e.user_id AS requester_user_id FROM payslip p JOIN payroll_cycle c ON c.id=p.payroll_cycle_id AND c.tenant_id=p.tenant_id JOIN employee e ON e.id=p.employee_id AND e.tenant_id=p.tenant_id AND NOT e.is_deleted WHERE p.tenant_id=$1 AND p.id=$2";
pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    match event.resource_type {
        EmailResourceType::LoanRequest => {
            let Some(row) = db
                .query_one(statement(
                    EMAIL_LOAN_SQL,
                    vec![event.tenant_id.into(), event.resource_id.into()],
                ))
                .await?
            else {
                return Ok(EmailEligibilityDecision::Obsolete);
            };
            let status: String = row.try_get("", "state")?;
            let active: String = row.try_get("", "employee_status")?;
            if event.kind==EmailEventKind::ActionRequired {
                let Some(workflow_id)=row.try_get::<Option<uuid::Uuid>>("", "workflow_id")? else {return Ok(EmailEligibilityDecision::Obsolete);};
                match kabipay_loans::configured_loan_approval_step(db,event.tenant_id,workflow_id).await {
                    Ok(_) => {},
                    Err(kabipay_loans::LoanModuleError::PolicyUnavailable) => return Ok(EmailEligibilityDecision::Obsolete),
                    Err(_) => return Err(kabipay_common::KabiPayError::Internal("loan approval definition lookup failed".into())),
                }
            }
            let subject = EmailSubjectSnapshot {
                employee_id: row.try_get("", "employee_id")?,
                requester_user_id: row.try_get("", "requester_user_id")?,
                resource_version: format!("{}:{}", status, row.try_get::<i64>("", "version")?),
                pending: matches!(status.as_str(), "SUBMITTED" | "UNDER_REVIEW")
                    && matches!(active.as_str(), "ACTIVE" | "PROBATION" | "ON_LEAVE"),
                outcome_is_current: email_outcome_matches(event, &status.to_ascii_lowercase())
                    && event.resource_version == format!("{}:{}",status,row.try_get::<i64>("", "version")?),
            };
            check_workflow_email(
                db,
                request,
                subject,
                WorkflowEmailResource {
                    instance_id: row.try_get("", "workflow_instance_id")?,
                    entity_type: "LOAN_REQUEST",
                    require_active_definition: true,
                },
            )
            .await
        }
        EmailResourceType::Payslip => {
            let Some(row) = db
                .query_one(statement(
                    EMAIL_PAYSLIP_SQL,
                    vec![event.tenant_id.into(), event.resource_id.into()],
                ))
                .await?
            else {
                return Ok(EmailEligibilityDecision::Obsolete);
            };
            let user: Option<uuid::Uuid> = row.try_get("", "requester_user_id")?;
            if !matches!(event.recipient,EmailRecipientRef::LocalUser{user_id} if Some(user_id)==user)
            {
                return Ok(EmailEligibilityDecision::Forbidden);
            }
            let status: String = row.try_get("", "status")?;
            let cycle: String = row.try_get("", "cycle_status")?;
            if event.kind != EmailEventKind::PayslipAvailable
                || status != "GENERATED"
                || cycle != "PROCESSED"
                || event.resource_version
                    != email_timestamp_version("GENERATED", row.try_get("", "updated_at")?)
            {
                return Ok(EmailEligibilityDecision::Obsolete);
            }
            Ok(EmailEligibilityDecision::Eligible)
        }
        _ => Ok(EmailEligibilityDecision::Forbidden),
    }
}
