//! Permission-protected mutations for workflow definitions.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;
use crate::resolvers::types::{
    CreateWorkflowInput, CreateWorkflowStepInput, WorkflowDto, WorkflowStepDto,
};
use crate::services::workflow_service;

pub struct MutationRoot;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

#[Object]
impl MutationRoot {
    /// Create a workflow **definition** row. Requires `workflow:manage`.
    async fn create_workflow(
        &self,
        ctx: &Context<'_>,
        input: CreateWorkflowInput,
    ) -> Result<WorkflowDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_workflow_definitions() {
            return Err(
                KabiPayError::Forbidden("missing permission to manage workflows".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(KabiPayError::Validation("name must not be empty".into()).into_graphql());
        }
        let entity_type = input.entity_type.trim().to_string();
        if entity_type.is_empty() {
            return Err(KabiPayError::Validation("entityType must not be empty".into()).into_graphql());
        }
        let m = workflow_service::create_workflow(
            &db,
            tenant_id,
            name,
            entity_type,
            input.is_active,
            input.initial_approver_type,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(WorkflowDto::from(m))
    }

    /// Add a **step** to an existing workflow. Requires `workflow:manage`.
    async fn create_workflow_step(
        &self,
        ctx: &Context<'_>,
        input: CreateWorkflowStepInput,
    ) -> Result<WorkflowStepDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_workflow_definitions() {
            return Err(
                KabiPayError::Forbidden("missing permission to manage workflows".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wf_id = parse_uuid(&input.workflow_id, "workflowId")?;
        let step_name = input.step_name.trim().to_string();
        if step_name.is_empty() {
            return Err(KabiPayError::Validation("stepName must not be empty".into()).into_graphql());
        }
        let role = input
            .approver_role_id
            .as_ref()
            .map(|id| parse_uuid(id, "approverRoleId"))
            .transpose()?;
        let m = workflow_service::create_workflow_step(
            &db,
            tenant_id,
            wf_id,
            input.sequence_order,
            step_name,
            input.approver_type,
            role,
            input.approver_permission,
            input.can_skip,
            input.sla_hours,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(WorkflowStepDto::from(m))
    }

    /// Delete a **workflow step** definition. Blocked if any **`workflow_action`** references this step.
    async fn delete_workflow_step(&self, ctx: &Context<'_>, step_id: ID) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_workflow_definitions() {
            return Err(
                KabiPayError::Forbidden("missing permission to manage workflows".into()).into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&step_id, "stepId")?;
        workflow_service::delete_workflow_step(&db, tenant_id, sid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Re-assign **`sequenceOrder`** across all steps for **`workflow_id`**. **`step_ids_ordered`** must list each step exactly once (same set as persisted).
    async fn reorder_workflow_steps(
        &self,
        ctx: &Context<'_>,
        workflow_id: ID,
        step_ids_ordered: Vec<ID>,
    ) -> Result<Vec<WorkflowStepDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_workflow_definitions() {
            return Err(
                KabiPayError::Forbidden("missing permission to manage workflows".into()).into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wf_id = parse_uuid(&workflow_id, "workflowId")?;
        let ordered = step_ids_ordered
            .iter()
            .map(|id| parse_uuid(id, "stepIdsOrdered"))
            .collect::<Result<Vec<_>>>()?;
        let rows =
            workflow_service::reorder_workflow_steps(&db, tenant_id, wf_id, ordered)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WorkflowStepDto::from).collect())
    }
}
