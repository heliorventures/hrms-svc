use kabipay_common::{tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};
use kabipay_organization_protocol::*;
use kabipay_organization_tenant::{
    statement, unavailable, EmptyPayload, SourceAdapter, WorkflowDecision,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;
#[derive(Clone)]
pub struct LeaveAdapter;
impl SourceAdapter for LeaveAdapter {
    fn service(&self) -> &'static str {
        "kabipay-leave"
    }
    async fn employee(
        &self,
        db: &DatabaseConnection,
        tenant: Uuid,
        c: &OrganizationCommand,
    ) -> KabiPayResult<Option<Uuid>> {
        let row=db.query_one(statement("SELECT employee_id FROM leave_request WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
        Ok(Some(row.try_get("", "employee_id")?))
    }
    async fn execute(
        &self,
        tx: &DatabaseTransaction,
        ops: &DatabaseConnection,
        p: &kabipay_organization_tenant::VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        match c.operation {
            OrganizationOperation::LeaveDetail => {
                let _: EmptyPayload = serde_json::from_value(c.payload.clone())?;
            }
            OrganizationOperation::LeaveDecision => {
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
                let date = TenantBusinessClock::load(ops, p.tenant_id)
                    .await?
                    .now_date();
                crate::services::leave_service::decide_organization_leave(
                    tx,
                    p,
                    c.resource_id,
                    date,
                    input,
                )
                .await?;
            }
            _ => return Err(unavailable()),
        }
        let row=tx.query_one(statement("SELECT r.id,r.employee_id,r.status,r.from_date,r.to_date,r.days_requested,r.reason,r.rejection_reason,i.current_step_id,v.version FROM leave_request r LEFT JOIN workflow_instance i ON i.tenant_id=r.tenant_id AND i.id=r.workflow_instance_id JOIN organization_resource_version v ON v.tenant_id=r.tenant_id AND v.resource_id=r.id AND v.resource_type='leave_request' WHERE r.tenant_id=$1 AND r.id=$2 AND NOT r.is_deleted",vec![p.tenant_id.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
        let mut result = serde_json::json!({"id":row.try_get::<Uuid>("","id")?,"employee_id":row.try_get::<Uuid>("","employee_id")?,"status":row.try_get::<String>("","status")?,"expected_workflow_step_id":row.try_get::<Option<Uuid>>("","current_step_id")?,"version":row.try_get::<i64>("","version")?});
        if c.operation == OrganizationOperation::LeaveDetail {
            let object = result.as_object_mut().ok_or_else(unavailable)?;
            object.insert(
                "from_date".into(),
                serde_json::json!(row.try_get::<chrono::NaiveDate>("", "from_date")?),
            );
            object.insert(
                "to_date".into(),
                serde_json::json!(row.try_get::<chrono::NaiveDate>("", "to_date")?),
            );
            object.insert(
                "days_requested".into(),
                serde_json::json!(row
                    .try_get::<rust_decimal::Decimal>("", "days_requested")?
                    .to_string()),
            );
            object.insert(
                "reason".into(),
                serde_json::json!(row.try_get::<Option<String>>("", "reason")?),
            );
            object.insert(
                "rejection_reason".into(),
                serde_json::json!(row.try_get::<Option<String>>("", "rejection_reason")?),
            );
        }
        Ok(result)
    }
}
