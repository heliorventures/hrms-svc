//! GraphQL DTOs for kabipay-workflow.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, Utc};
use kabipay_db_entities::tenant::d0025_workflow::{workflow, workflow_instance, workflow_step};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Workflow")]
pub struct WorkflowDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub entity_type: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<workflow::Model> for WorkflowDto {
    fn from(m: workflow::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            entity_type: m.entity_type,
            is_active: m.is_active,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WorkflowInstance")]
pub struct WorkflowInstanceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub workflow_id: ID,
    pub entity_type: String,
    pub entity_id: ID,
    pub status: String,
    pub current_step_id: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<workflow_instance::Model> for WorkflowInstanceDto {
    fn from(m: workflow_instance::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            workflow_id: ID(m.workflow_id.to_string()),
            entity_type: m.entity_type,
            entity_id: ID(m.entity_id.to_string()),
            status: m.status,
            current_step_id: m.current_step_id.map(|u| ID(u.to_string())),
            created_at: m.created_at,
            completed_at: m.completed_at,
        }
    }
}

/// One node in a workflow graph (**reorder** + **delete step** exposed to admins; richer designer later).
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WorkflowStep")]
pub struct WorkflowStepDto {
    pub id: ID,
    pub tenant_id: ID,
    pub workflow_id: ID,
    pub sequence_order: i32,
    pub step_name: String,
    pub approver_type: Option<String>,
    pub approver_role_id: Option<ID>,
    pub approver_permission: Option<String>,
    pub can_skip: bool,
    pub sla_hours: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<workflow_step::Model> for WorkflowStepDto {
    fn from(m: workflow_step::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            workflow_id: ID(m.workflow_id.to_string()),
            sequence_order: m.sequence_order,
            step_name: m.step_name,
            approver_type: m.approver_type,
            approver_role_id: m.approver_role_id.map(|u| ID(u.to_string())),
            approver_permission: m.approver_permission,
            can_skip: m.can_skip,
            sla_hours: m.sla_hours,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Workflow definition + ordered steps (for a designer-style board).
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WorkflowWithSteps")]
pub struct WorkflowWithStepsDto {
    pub workflow: WorkflowDto,
    pub steps: Vec<WorkflowStepDto>,
}

/// Create a new workflow **definition** (e.g. `LEAVE_REQUEST`, `EXPENSE`).
#[derive(InputObject, Clone, Debug)]
pub struct CreateWorkflowInput {
    pub name: String,
    /// Typically `LEAVE_REQUEST`, `EXPENSE`, etc. (must match runtime consumers).
    pub entity_type: String,
    #[graphql(default = true)]
    pub is_active: bool,
}

/// Add a **step** to a workflow. `sequence_order` must be unique per workflow.
#[derive(InputObject, Clone, Debug)]
pub struct CreateWorkflowStepInput {
    pub workflow_id: ID,
    pub sequence_order: i32,
    pub step_name: String,
    pub approver_type: Option<String>,
    pub approver_role_id: Option<ID>,
    pub approver_permission: Option<String>,
    #[graphql(default = false)]
    pub can_skip: bool,
    pub sla_hours: Option<i32>,
}
