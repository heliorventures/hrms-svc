use serde::Deserialize;
use uuid::Uuid;
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDecision {
    pub decision: Decision,
    pub expected_workflow_step_id: Uuid,
    pub rejection_reason: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyPayload {}
