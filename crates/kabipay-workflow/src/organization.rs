//! Fixed-source workflow definition administration; approval execution remains domain owned.
use crate::services::workflow_service as domain;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{
    AuthorityKind, OrganizationCommand, OrganizationOperation as Op,
};
use kabipay_organization_tenant::{
    statement, unavailable, SourceAdapter, VerifiedRuntimeAuthorization,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct WorkflowAdapter;
impl SourceAdapter for WorkflowAdapter {
    fn service(&self) -> &'static str {
        "kabipay-workflow"
    }
    async fn employee(
        &self,
        _: &DatabaseConnection,
        _: Uuid,
        _: &OrganizationCommand,
    ) -> KabiPayResult<Option<Uuid>> {
        Ok(None)
    }
    async fn execute(
        &self,
        db: &DatabaseTransaction,
        _: &DatabaseConnection,
        p: &VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        if p.authority_kind != AuthorityKind::OrganizationGrant
            || p.employee_scope != "ALL"
            || p.capability != "workflow:manage"
        {
            return Err(unavailable());
        }
        if c.operation == Op::WorkflowList {
            if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
                return Err(unavailable());
            }
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Page {
                after: Option<Uuid>,
                first: u32,
            }
            let i: Page = serde_json::from_value(c.payload.clone())?;
            if i.first == 0 || i.first > 100 {
                return Err(KabiPayError::Validation(
                    "Workflow page size must be between 1 and 100".into(),
                ));
            }
            let rows=db.query_all(statement("SELECT w.id,w.name,w.entity_type,w.is_active,v.version FROM workflow w JOIN organization_resource_version v ON v.tenant_id=w.tenant_id AND v.resource_type='workflow_definition' AND v.resource_id=w.id WHERE w.tenant_id=$1 AND ($2::uuid IS NULL OR w.id>$2) ORDER BY w.id LIMIT $3",vec![p.tenant_id.into(),i.after.into(),i64::from(i.first).into()])).await?;
            let mut workflows = Vec::new();
            for r in rows {
                workflows.push(serde_json::json!({"workflow_id":r.try_get::<Uuid>("","id")?,"name":r.try_get::<String>("","name")?,"entity_type":r.try_get::<String>("","entity_type")?,"is_active":r.try_get::<bool>("","is_active")?,"version":r.try_get::<i64>("","version")?}));
            }
            return Ok(
                serde_json::json!({"after":workflows.last().and_then(|w|w.get("workflow_id")),"has_more":workflows.len()==i.first as usize,"workflows":workflows}),
            );
        }
        match c.operation {
            Op::WorkflowDetail => {
                let _: kabipay_organization_tenant::EmptyPayload =
                    serde_json::from_value(c.payload.clone())?;
            }
            Op::WorkflowSave => {
                let i: domain::DefinitionInput = serde_json::from_value(c.payload.clone())?;
                domain::save_definition(db, p.tenant_id, c.resource_id, i).await?;
            }
            Op::WorkflowActivation => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    is_active: bool,
                }
                let i: Input = serde_json::from_value(c.payload.clone())?;
                domain::set_definition_active(db, p.tenant_id, c.resource_id, i.is_active).await?;
            }
            _ => return Err(unavailable()),
        }
        let w = domain::get_workflow(db, p.tenant_id, c.resource_id)
            .await?
            .ok_or_else(unavailable)?;
        let steps=domain::list_workflow_steps(db,p.tenant_id,c.resource_id).await?.into_iter().map(|s|serde_json::json!({"step_id":s.id,"sequence_order":s.sequence_order,"step_name":s.step_name,"approver_type":s.approver_type,"approver_permission":s.approver_permission,"can_skip":s.can_skip,"sla_hours":s.sla_hours})).collect::<Vec<_>>();
        Ok(
            serde_json::json!({"workflow_id":w.id,"name":w.name,"entity_type":w.entity_type,"is_active":w.is_active,"steps":steps}),
        )
    }
}
