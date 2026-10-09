use crate::{
    access, arrangement, lifecycle, payments, repository as store, CommandMeta, LoanActorScope,
    LoanCommand, LoanCommandResult, LoanModuleError, LoanResult,
};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

/// The caller must roll back on error and commit only after all participating writes succeed.
/// This module does not create connections, commit, or perform network operations.
pub async fn execute_command(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    meta: &CommandMeta,
    input: &LoanCommand,
) -> LoanResult<LoanCommandResult> {
    meta.validate()?;
    if actor.permission != input.permission() {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    store::synchronize(tx, actor.tenant_id).await?;
    let payload = serde_json::to_value(input)?;
    let action = payload
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or(LoanModuleError::InvalidCommand)?;
    let hash = crate::command_hash(&(meta.expected_version, input))?;
    if let Some(row)=store::one(tx,"SELECT command,payload_hash,result FROM loan_command_receipt WHERE tenant_id=$1 AND actor_id=$2 AND idempotency_key=$3",vec![actor.tenant_id.into(),actor.user_id.into(),meta.idempotency_key.clone().into()]).await? {
        let recorded:String=row.try_get("","payload_hash")?;let command:String=row.try_get("","command")?;
        if recorded!=hash || command!=action{return Err(LoanModuleError::IdempotencyConflict)}
        let result:LoanCommandResult=serde_json::from_value(row.try_get("","result")?)?;
        if let Some(employee)=result.employee_id{access::employee(tx,actor,employee).await?;}else{access::company(actor)?;}
        return Ok(result)
    }
    let expected = meta.expected_version;
    let result = match input {
        LoanCommand::PublishPolicy {
            key,
            currency,
            effective_from,
            effective_to,
            rules,
        } => {
            lifecycle::publish(
                tx,
                actor,
                expected,
                key,
                currency,
                *effective_from,
                *effective_to,
                rules,
            )
            .await?
        }
        LoanCommand::RetirePolicy { policy_id, reason } => {
            lifecycle::retire_policy(tx, actor, expected, *policy_id, reason).await?
        }
        LoanCommand::SaveRequest {
            request_id,
            employee_id,
            policy_id,
            amount,
            purpose,
            notes,
            preferences,
        }
        | LoanCommand::SubmitRequest {
            request_id,
            employee_id,
            policy_id,
            amount,
            purpose,
            notes,
            preferences,
        } => {
            lifecycle::submit(
                tx,
                actor,
                expected,
                *request_id,
                *employee_id,
                *policy_id,
                *amount,
                purpose,
                notes,
                preferences,
                if matches!(input, LoanCommand::SaveRequest { .. }) {
                    lifecycle::RequestStage::Draft
                } else {
                    lifecycle::RequestStage::Submitted
                },
            )
            .await?
        }
        LoanCommand::DecideRequest {
            request_id,
            step_id,
            decision,
            reason,
            terms,
            effective_from,
            first_due_date,
            agreement_reference,
        } => {
            lifecycle::decide(
                tx,
                actor,
                expected,
                *request_id,
                *step_id,
                *decision,
                reason,
                terms.as_ref(),
                *effective_from,
                *first_due_date,
                agreement_reference.as_deref(),
            )
            .await?
        }
        LoanCommand::WithdrawRequest { request_id, reason } => {
            lifecycle::withdraw(tx, actor, expected, *request_id, reason).await?
        }
        LoanCommand::RecordDisbursement { loan_id, payment } => {
            payments::disburse(tx, actor, *loan_id, expected, payment).await?
        }
        LoanCommand::RecordReceipt { loan_id, payment } => {
            payments::receipt(tx, actor, *loan_id, expected, payment).await?
        }
        LoanCommand::SetDeduction {
            loan_id,
            effective_from,
            first_due_date,
            amount,
            recovery,
            reason,
        } => {
            arrangement::deduction(
                tx,
                actor,
                expected,
                *loan_id,
                *effective_from,
                *first_due_date,
                *amount,
                *recovery,
                reason,
            )
            .await?
        }
        LoanCommand::SetPeriodOverride {
            loan_id,
            period_start,
            amount,
            pause_interest,
            reason,
        } => {
            arrangement::period(
                tx,
                actor,
                expected,
                *loan_id,
                *period_start,
                *amount,
                *pause_interest,
                reason,
            )
            .await?
        }
        LoanCommand::ReversePosting {
            loan_id,
            posting_id,
            reason,
            reconciliation_reference,
            review_fingerprint,
        } => {
            crate::correction::reverse(
                tx,
                actor,
                expected,
                *loan_id,
                *posting_id,
                reason,
                reconciliation_reference,
                review_fingerprint.as_deref(),
            )
            .await?
        }
    };
    if let Some(employee) = result.employee_id {
        store::advance_revision(tx, actor.tenant_id, employee, action).await?;
    }
    let reason = payload
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    store::execute(tx,"INSERT INTO loan_audit_event(id,tenant_id,loan_id,employee_id,actor_id,action,before_version,after_version,reason,\"references\") VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),result.loan_id.into(),result.employee_id.into(),actor.user_id.into(),action.into(),expected.into(),result.version.into(),reason.into(),serde_json::json!({"recordId":result.record_id,"postingIds":result.posting_ids}).into()]).await?;
    store::execute(tx,"INSERT INTO loan_command_receipt(id,tenant_id,actor_id,idempotency_key,command,payload_hash,result) VALUES($1,$2,$3,$4,$5,$6,$7)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),actor.user_id.into(),meta.idempotency_key.clone().into(),action.into(),hash.into(),serde_json::to_value(&result)?.into()]).await?;
    Ok(result)
}
