//! Root query resolvers for kabipay-workflow.

use async_graphql::{Context, ID, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use uuid::Uuid;

use crate::resolvers::types::{WorkflowDto, WorkflowInstanceDto, WorkflowWithStepsDto, WorkflowStepDto};
use crate::services::workflow_service;

pub struct QueryRoot;

fn require_workflow_management(ctx: &Context<'_>) -> Result<()> {
    if !require_client_claims(ctx)?.can_manage_workflow_definitions() {
        return Err(KabiPayError::Forbidden(
            "workflow:manage permission requires ALL scope".into(),
        )
        .into_graphql());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvers::mutation::MutationRoot;
    use async_graphql::{EmptySubscription, Schema};
    use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;

    #[tokio::test]
    async fn tenant_workflow_reads_require_manage_all_before_database_access() {
        for scope in [
            None,
            Some("invalid"),
            Some("SELF"),
            Some("TEAM"),
            Some("DEPARTMENT"),
            Some("ALL"),
        ] {
            let tenant_id = Uuid::new_v4();
            let claims = ClientClaims {
                sub: Uuid::new_v4(),
                iss: CLIENT_JWT_ISSUER.into(),
                exp: 0,
                iat: 0,
                tenant_id,
                email: String::new(),
                employee_id: None,
                must_change_password: false,
                roles: vec![],
                permissions: vec!["workflow:manage".into()],
                permission_scopes: scope
                    .map(|s| HashMap::from([("workflow:manage".into(), s.into())]))
                    .unwrap_or_default(),
                resource_scopes: HashMap::new(),
            };
            let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
                .data(TenantId(tenant_id))
                .data(claims)
                .finish();
            for query in [
                "{ workflows { id } }",
                "{ workflowInstances { id } }",
                "{ workflowsWithSteps { workflow { id } } }",
                "{ workflowSteps(workflowId: \"00000000-0000-0000-0000-000000000001\") { id } }",
            ] {
                let response = schema.execute(query).await;
                assert_eq!(response.errors.len(), 1, "{response:?}");
                let message = &response.errors[0].message;
                if scope == Some("ALL") {
                    assert!(message == "internal server error", "{query}: {message}");
                } else {
                    assert!(
                        message.contains("workflow:manage"),
                        "{query} {scope:?}: {message}"
                    );
                }
            }
            for mutation in [
                "mutation { createWorkflow(input: {name: \"Test\", entityType: \"LEAVE_REQUEST\"}) { id } }",
                "mutation { createWorkflowStep(input: {workflowId: \"00000000-0000-0000-0000-000000000001\", sequenceOrder: 1, stepName: \"Test\"}) { id } }",
                "mutation { deleteWorkflowStep(stepId: \"00000000-0000-0000-0000-000000000001\") }",
                "mutation { reorderWorkflowSteps(workflowId: \"00000000-0000-0000-0000-000000000001\", stepIdsOrdered: []) { id } }",
            ] {
                let response = schema.execute(mutation).await;
                assert_eq!(response.errors.len(), 1, "{response:?}");
                let message = &response.errors[0].message;
                assert_eq!(message == "internal server error", scope == Some("ALL"), "{mutation} {scope:?}: {message}");
                if scope != Some("ALL") { assert!(message.contains("permission"), "{message}"); }
            }
        }
    }
}

#[Object]
impl QueryRoot {
    async fn workflow_health(&self) -> &'static str {
        "ok"
    }

    async fn workflows(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<WorkflowDto>> {
        require_workflow_management(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = workflow_service::list_workflows(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WorkflowDto::from).collect())
    }

    async fn workflow_instances(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<WorkflowInstanceDto>> {
        require_workflow_management(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = workflow_service::list_instances(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WorkflowInstanceDto::from).collect())
    }

    /// All active workflow definitions, each with ordered steps (read-only “designer” data).
    async fn workflows_with_steps(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<WorkflowWithStepsDto>> {
        require_workflow_management(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wfs = workflow_service::list_workflows(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let mut out: Vec<WorkflowWithStepsDto> = Vec::with_capacity(wfs.len());
        for w in wfs {
            let steps = workflow_service::list_workflow_steps(&db, tenant_id, w.id)
                .await
                .map_err(KabiPayError::into_graphql)?
                .into_iter()
                .map(WorkflowStepDto::from)
                .collect();
            out.push(WorkflowWithStepsDto {
                workflow: WorkflowDto::from(w),
                steps,
            });
        }
        Ok(out)
    }

    /// Step list for a single workflow (same ordering as `workflowsWithSteps` per workflow).
    async fn workflow_steps(
        &self,
        ctx: &Context<'_>,
        workflow_id: ID,
    ) -> Result<Vec<WorkflowStepDto>> {
        require_workflow_management(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = Uuid::parse_str(workflow_id.as_str())
            .map_err(|e| {
                KabiPayError::Validation(format!("invalid workflowId: {e}")).into_graphql()
            })?;
        let steps = workflow_service::list_workflow_steps(&db, tenant_id, id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(steps.into_iter().map(WorkflowStepDto::from).collect())
    }
}
