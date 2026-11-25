//! Current timesheet approval and authoritative missed-punch expiry metadata.
use kabipay_common::{important_email::*, KabiPayResult};
use kabipay_organization_tenant::{
    email_routes::EmailDomainAdapter,
    email_workflow::{check_workflow_email, WorkflowEmailResource},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
#[derive(Clone)]
pub struct AttendanceEmailAdapter;
impl EmailDomainAdapter for AttendanceEmailAdapter {
    fn domain(&self) -> &'static str {
        "attendance"
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
pub const EMAIL_TIMESHEET_SQL:&str="SELECT r.employee_id,r.status,r.updated_at,r.workflow_instance_id,e.user_id AS requester_user_id,e.status AS employee_status FROM timesheet_week_batch r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id AND NOT e.is_deleted WHERE r.tenant_id=$1 AND r.id=$2";
pub const EMAIL_ATTENDANCE_SQL:&str="SELECT a.status,a.regularization_status,a.updated_at,(a.check_out_at IS NULL AND a.check_out_time IS NULL) AS missing_checkout,e.user_id AS requester_user_id FROM attendance a JOIN employee e ON e.id=a.employee_id AND e.tenant_id=a.tenant_id AND NOT e.is_deleted WHERE a.tenant_id=$1 AND a.id=$2";
pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    match event.resource_type {
        EmailResourceType::Timesheet => {
            let Some(row) = db
                .query_one(statement(
                    EMAIL_TIMESHEET_SQL,
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
                outcome_is_current: workflow_email_outcome_is_current(db,event,row.try_get("", "workflow_instance_id")?,crate::services::timesheet_batch_service::WF_ENTITY_TIMESHEET_WEEK_BATCH,&status,row.try_get("", "updated_at")?).await?,
            };
            check_workflow_email(
                db,
                request,
                subject,
                WorkflowEmailResource {
                    instance_id: row.try_get("", "workflow_instance_id")?,
                    entity_type:
                        crate::services::timesheet_batch_service::WF_ENTITY_TIMESHEET_WEEK_BATCH,
                    require_active_definition: false,
                },
            )
            .await
        }
        EmailResourceType::Attendance => {
            let Some(row) = db
                .query_one(statement(
                    EMAIL_ATTENDANCE_SQL,
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
            let reason: Option<String> = row.try_get("", "regularization_status")?;
            if event.kind != EmailEventKind::MissedPunchOut
                || status != "INCOMPLETE"
                || reason.as_deref() != Some("MISSED_PUNCH_OUT")
                || !row.try_get::<bool>("", "missing_checkout")?
                || event.resource_version
                    != email_timestamp_version("MISSED_PUNCH_OUT", row.try_get("", "updated_at")?)
            {
                return Ok(EmailEligibilityDecision::Obsolete);
            }
            Ok(EmailEligibilityDecision::Eligible)
        }
        _ => Ok(EmailEligibilityDecision::Forbidden),
    }
}
