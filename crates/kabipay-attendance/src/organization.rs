use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::*;
use kabipay_organization_tenant::{
    statement, unavailable, EmptyPayload, SourceAdapter, WorkflowDecision,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;
#[derive(Clone)]
pub struct AttendanceAdapter;
impl SourceAdapter for AttendanceAdapter {
    fn service(&self) -> &'static str {
        "kabipay-attendance"
    }
    async fn employee(
        &self,
        db: &DatabaseConnection,
        tenant: Uuid,
        c: &OrganizationCommand,
    ) -> KabiPayResult<Option<Uuid>> {
        let row = db
            .query_one(statement(
                "SELECT employee_id FROM timesheet_week_batch WHERE tenant_id=$1 AND id=$2",
                vec![tenant.into(), c.resource_id.into()],
            ))
            .await?
            .ok_or_else(unavailable)?;
        Ok(Some(row.try_get("", "employee_id")?))
    }
    async fn execute(
        &self,
        tx: &DatabaseTransaction,
        _: &DatabaseConnection,
        p: &kabipay_organization_tenant::VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        match c.operation {
            OrganizationOperation::TimesheetDetail => {
                let _: EmptyPayload = serde_json::from_value(c.payload.clone())?;
            }
            OrganizationOperation::TimesheetDecision => {
                let input: WorkflowDecision = serde_json::from_value(c.payload.clone())?;
                if input
                    .rejection_reason
                    .as_ref()
                    .is_some_and(|r| r.len() > 4000)
                {
                    return Err(KabiPayError::Validation(
                        "Decision reason exceeds 4000 characters".into(),
                    ));
                }
                crate::services::timesheet_batch_service::decide_organization_timesheet(
                    tx,
                    p,
                    c.resource_id,
                    input,
                )
                .await?;
            }
            _ => return Err(unavailable()),
        }
        let row=tx.query_one(statement("SELECT b.id,b.employee_id,b.status,b.week_start_date,b.rejection_reason,i.current_step_id,v.version FROM timesheet_week_batch b LEFT JOIN workflow_instance i ON i.tenant_id=b.tenant_id AND i.id=b.workflow_instance_id JOIN organization_resource_version v ON v.tenant_id=b.tenant_id AND v.resource_id=b.id AND v.resource_type='timesheet' WHERE b.tenant_id=$1 AND b.id=$2",vec![p.tenant_id.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
        let mut result = serde_json::json!({"id":row.try_get::<Uuid>("","id")?,"employee_id":row.try_get::<Uuid>("","employee_id")?,"status":row.try_get::<String>("","status")?,"week_start_date":row.try_get::<chrono::NaiveDate>("","week_start_date")?,"expected_workflow_step_id":row.try_get::<Option<Uuid>>("","current_step_id")?,"version":row.try_get::<i64>("","version")?});
        if c.operation == OrganizationOperation::TimesheetDetail {
            let rows=tx.query_all(statement("SELECT id,work_date,hours_worked,project_code,description,status FROM timesheet_entry WHERE tenant_id=$1 AND batch_id=$2 AND NOT is_deleted ORDER BY work_date,id",vec![p.tenant_id.into(),c.resource_id.into()])).await?;
            let mut entries = Vec::new();
            for r in rows {
                entries.push(serde_json::json!({"id":r.try_get::<Uuid>("","id")?,"work_date":r.try_get::<chrono::NaiveDate>("","work_date")?,"hours_worked":r.try_get::<rust_decimal::Decimal>("","hours_worked")?.to_string(),"project_code":r.try_get::<Option<String>>("","project_code")?,"description":r.try_get::<Option<String>>("","description")?,"status":r.try_get::<String>("","status")?}));
            }
            result
                .as_object_mut()
                .ok_or_else(unavailable)?
                .insert("entries".into(), serde_json::json!(entries));
        }
        Ok(result)
    }
}
