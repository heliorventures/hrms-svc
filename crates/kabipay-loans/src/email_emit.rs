//! New committed request transitions only. Durable command replay exits before
//! this producer; draft edits and ledger/posting operations are deliberately silent.
use crate::{LoanActorScope, LoanCommand, LoanCommandResult, LoanModuleError, LoanResult};
use kabipay_common::important_email::*;
use kabipay_organization_tenant::{
    email_emission::{emit_requester_outcome, emit_workflow_actions, WorkflowEmailActivation},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
pub async fn emit_loan_email(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: &LoanCommand,
    result: &LoanCommandResult,
) -> LoanResult<usize> {
    if !matches!(
        input,
        LoanCommand::SubmitRequest { .. }
            | LoanCommand::DecideRequest { .. }
            | LoanCommand::WithdrawRequest { .. }
    ) {
        return Ok(0);
    }
    let row=tx.query_one(statement("SELECT employee_id,state,version,workflow_instance_id,CURRENT_TIMESTAMP AS event_at FROM loan_request WHERE tenant_id=$1 AND id=$2",vec![actor.tenant_id.into(),result.record_id.into()])).await?.ok_or(LoanModuleError::NotFound)?;
    let state: String = row.try_get("", "state")?;
    let version: i64 = row.try_get("", "version")?;
    let transition = EmailTransition {
        tenant_id: actor.tenant_id,
        resource_type: EmailResourceType::LoanRequest,
        resource_id: result.record_id,
        resource_version: format!("{state}:{version}"),
        occurred_at: row.try_get("", "event_at")?,
        actor_ref: Some(EmailActorRef::LocalUser {
            user_id: actor.local_user()?,
        }),
        actor_target_user_ids: vec![],
    };
    let employee = row.try_get("", "employee_id")?;
    if matches!(state.as_str(), "SUBMITTED" | "UNDER_REVIEW") {
        Ok(emit_workflow_actions(
            tx,
            None,
            WorkflowEmailActivation {
                transition,
                employee_id: employee,
                instance_id: row.try_get("", "workflow_instance_id")?,
                entity_type: "LOAN_REQUEST",
                require_active_definition: true,
            },
        )
        .await?)
    } else if matches!(
        state.as_str(),
        "APPROVED" | "RETURNED" | "REJECTED" | "WITHDRAWN"
    ) {
        Ok(emit_requester_outcome(tx, transition, employee, state.to_ascii_lowercase()).await?)
    } else {
        Ok(0)
    }
}
