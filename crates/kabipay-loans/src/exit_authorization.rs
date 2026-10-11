//! Durable Loan approval guard. Only authoritative HR outcomes release it.
use crate::{
    access, preview_exit_exposure, repository as store, validate_exit_decision,
    ExitFinancialDecision, LoanActorScope, LoanExitExposure, LoanModuleError, LoanPermission,
    LoanResult,
};
use kabipay_common::finance_contract::{
    reconcile_authorization, FinanceAuthorizationState, HrOperationState,
};
use sea_orm::DatabaseTransaction;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveExitReview {
    pub separation_id: Uuid,
    pub employee_id: Uuid,
    pub separation_fingerprint: String,
    pub last_working_date: chrono::NaiveDate,
    pub settlement_date: chrono::NaiveDate,
    pub exposure_fingerprint: String,
    pub decision: ExitFinancialDecision,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedExitReview {
    pub id: Uuid,
    pub separation_id: Uuid,
    pub separation_fingerprint: String,
    pub exposure: LoanExitExposure,
    pub decision: ExitFinancialDecision,
}

pub async fn save_exit_review(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: &SaveExitReview,
) -> LoanResult<SavedExitReview> {
    if !matches!(
        actor.permission,
        LoanPermission::ExitReview | LoanPermission::FnfRecovery
    ) || input.separation_id.is_nil()
        || input.separation_fingerprint.len() != 64
    {
        return Err(LoanModuleError::Forbidden);
    }
    let exposure =
        preview_exit_exposure(tx, actor, input.employee_id, input.settlement_date).await?;
    if actor.permission == LoanPermission::FnfRecovery && !exposure.loans.is_empty() {
        return Err(LoanModuleError::Forbidden);
    }
    if exposure.fingerprint != input.exposure_fingerprint {
        return Err(LoanModuleError::VersionConflict);
    }
    validate_exit_decision(&exposure, &input.decision)?;
    let id = Uuid::new_v4();
    let hash = crate::command_hash(&input.decision)?;
    store::execute(tx,"INSERT INTO loan_exit_review(id,tenant_id,separation_id,employee_id,actor_id,separation_fingerprint,last_working_date,settlement_date,employee_revision,quote_fingerprint,preview,decision,decision_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",vec![id.into(),actor.tenant_id.into(),input.separation_id.into(),input.employee_id.into(),actor.user_id.into(),input.separation_fingerprint.clone().into(),input.last_working_date.into(),input.settlement_date.into(),exposure.employee_revision.into(),exposure.fingerprint.clone().into(),serde_json::to_value(&exposure)?.into(),serde_json::to_value(&input.decision)?.into(),hash.into()]).await?;
    Ok(SavedExitReview {
        id,
        separation_id: input.separation_id,
        separation_fingerprint: input.separation_fingerprint.clone(),
        exposure,
        decision: input.decision.clone(),
    })
}

pub async fn find_exit_review(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    id: Uuid,
) -> LoanResult<SavedExitReview> {
    if !matches!(
        actor.permission,
        LoanPermission::ExitReview | LoanPermission::FnfRecovery
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    access::company(actor)?;
    if actor.permission == LoanPermission::FnfRecovery {
        crate::payroll_loan_state(tx, actor.tenant_id).await?;
    } else {
        access::entitled(tx, actor).await?;
    }
    let row=store::one(tx,"SELECT separation_id,separation_fingerprint,preview,decision FROM loan_exit_review WHERE tenant_id=$1 AND id=$2",vec![actor.tenant_id.into(),id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let exposure: LoanExitExposure = serde_json::from_value(row.try_get("", "preview")?)?;
    access::employee(tx, actor, exposure.employee_id).await?;
    Ok(SavedExitReview {
        id,
        separation_id: row.try_get("", "separation_id")?,
        separation_fingerprint: row.try_get("", "separation_fingerprint")?,
        exposure,
        decision: serde_json::from_value(row.try_get("", "decision")?)?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisteredHrApproval {
    pub operation_id: Uuid,
    pub separation_id: Uuid,
    pub employee_id: Uuid,
    pub review_id: Uuid,
    pub actor_id: Uuid,
    pub separation_fingerprint: String,
    pub payload_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitAuthorization {
    pub id: Uuid,
    pub operation_id: Uuid,
    pub review_id: Uuid,
    pub employee_id: Uuid,
    pub separation_id: Uuid,
    pub actor_id: Uuid,
    pub payload_hash: String,
    pub state: FinanceAuthorizationState,
}

pub async fn exit_authorization(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    operation: Uuid,
) -> LoanResult<Option<ExitAuthorization>> {
    let row=store::one(tx,"SELECT id,operation_id,review_id,employee_id,separation_id,actor_id,payload_hash,state FROM loan_exit_approval_guard WHERE tenant_id=$1 AND operation_id=$2 FOR UPDATE",vec![tenant.into(),operation.into()]).await?;
    row.map(|r| {
        Ok(ExitAuthorization {
            id: r.try_get("", "id")?,
            operation_id: r.try_get("", "operation_id")?,
            review_id: r.try_get("", "review_id")?,
            employee_id: r.try_get("", "employee_id")?,
            separation_id: r.try_get("", "separation_id")?,
            actor_id: r.try_get("", "actor_id")?,
            payload_hash: r.try_get("", "payload_hash")?,
            state: serde_json::from_value(serde_json::Value::String(r.try_get("", "state")?))?,
        })
    })
    .transpose()
}

pub async fn authorize_exit_approval(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: &RegisteredHrApproval,
) -> LoanResult<ExitAuthorization> {
    if actor.permission != LoanPermission::FnfRecovery
        || input.actor_id != actor.user_id
        || input.operation_id.is_nil()
        || input.payload_hash.len() != 64
    {
        return Err(LoanModuleError::Forbidden);
    }
    access::company(actor)?;
    crate::payroll_loan_state(tx, actor.tenant_id).await?;
    store::synchronize(tx, actor.tenant_id).await?;
    if let Some(existing) = exit_authorization(tx, actor.tenant_id, input.operation_id).await? {
        if existing.payload_hash != input.payload_hash
            || existing.review_id != input.review_id
            || existing.employee_id != input.employee_id
            || existing.separation_id != input.separation_id
            || existing.actor_id != actor.user_id
        {
            return Err(LoanModuleError::IdempotencyConflict);
        }
        return Ok(existing);
    }
    let review = find_exit_review(tx, actor, input.review_id).await?;
    if review.separation_id != input.separation_id
        || review.exposure.employee_id != input.employee_id
        || review.separation_fingerprint != input.separation_fingerprint
    {
        return Err(LoanModuleError::VersionConflict);
    }
    let current = preview_exit_exposure(
        tx,
        actor,
        input.employee_id,
        review.exposure.settlement_date,
    )
    .await?;
    if current.fingerprint != review.exposure.fingerprint {
        return Err(LoanModuleError::VersionConflict);
    }
    validate_exit_decision(&current, &review.decision)?;
    let held=store::one(tx,"SELECT id FROM loan_exit_approval_guard WHERE tenant_id=$1 AND employee_id=$2 AND state IN ('AUTHORIZED','RECONCILIATION_PENDING')",vec![actor.tenant_id.into(),input.employee_id.into()]).await?;
    if held.is_some() {
        return Err(LoanModuleError::ExitReviewPending);
    }
    let id = Uuid::new_v4();
    store::execute(tx,"INSERT INTO loan_exit_approval_guard(id,tenant_id,employee_id,separation_id,review_id,operation_id,actor_id,payload_hash,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'AUTHORIZED',NOW()+INTERVAL '2 minutes')",vec![id.into(),actor.tenant_id.into(),input.employee_id.into(),input.separation_id.into(),input.review_id.into(),input.operation_id.into(),actor.user_id.into(),input.payload_hash.clone().into()]).await?;
    exit_authorization(tx, actor.tenant_id, input.operation_id)
        .await?
        .ok_or(LoanModuleError::NotFound)
}

/// The hosting service obtains this status from the authenticated Employee API before
/// opening the financial transaction. Missing/unreachable status is None, never cancellation.
pub async fn reconcile_exit_approval(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    input: &RegisteredHrApproval,
    status: Option<HrOperationState>,
) -> LoanResult<ExitAuthorization> {
    store::synchronize(tx, tenant).await?;
    let authorization = exit_authorization(tx, tenant, input.operation_id)
        .await?
        .ok_or(LoanModuleError::NotFound)?;
    if authorization.review_id != input.review_id
        || authorization.actor_id != input.actor_id
        || authorization.employee_id != input.employee_id
        || authorization.separation_id != input.separation_id
        || authorization.payload_hash != input.payload_hash
    {
        return Err(LoanModuleError::IdempotencyConflict);
    }
    let next = reconcile_authorization(authorization.state, status)?;
    let state = serde_json::to_value(next)?
        .as_str()
        .ok_or(LoanModuleError::InvalidCommand)?
        .to_string();
    store::execute(tx,"UPDATE loan_exit_approval_guard SET state=$3,confirmed_at=CASE WHEN $3='CONFIRMED' THEN COALESCE(confirmed_at,NOW()) ELSE confirmed_at END,updated_at=NOW() WHERE tenant_id=$1 AND operation_id=$2",vec![tenant.into(),input.operation_id.into(),state.into()]).await?;
    if next == FinanceAuthorizationState::Confirmed {
        let review = store::one(tx,"SELECT last_working_date,settlement_date,decision FROM loan_exit_review WHERE tenant_id=$1 AND id=$2",
            vec![tenant.into(),input.review_id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
        let decision = serde_json::from_value(review.try_get("","decision")?)?;
        crate::exit_servicing::record(tx,tenant,input.employee_id,input.separation_id,"HR_APPROVAL",input.review_id,input.actor_id,
            review.try_get("","last_working_date")?,review.try_get("","settlement_date")?,&decision).await?;
    }
    exit_authorization(tx, tenant, input.operation_id)
        .await?
        .ok_or(LoanModuleError::NotFound)
}

/// Private worker read model; no Loan table access escapes this module.
pub async fn pending_exit_approvals(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    first: u32,
) -> LoanResult<Vec<RegisteredHrApproval>> {
    if tenant.is_nil() || first == 0 || first > 50 {
        return Err(LoanModuleError::InvalidCommand);
    }
    let rows = store::all(tx,
        "SELECT g.operation_id,g.separation_id,g.employee_id,g.review_id,g.actor_id,g.payload_hash,r.separation_fingerprint FROM loan_exit_approval_guard g JOIN loan_exit_review r ON r.tenant_id=g.tenant_id AND r.id=g.review_id WHERE g.tenant_id=$1 AND g.state IN ('AUTHORIZED','RECONCILIATION_PENDING') AND g.expires_at<=NOW() ORDER BY g.updated_at,g.id LIMIT $2",
        vec![tenant.into(),i64::from(first).into()]).await?;
    rows.into_iter()
        .map(|r| {
            Ok(RegisteredHrApproval {
                operation_id: r.try_get("", "operation_id")?,
                separation_id: r.try_get("", "separation_id")?,
                employee_id: r.try_get("", "employee_id")?,
                review_id: r.try_get("", "review_id")?,
                actor_id: r.try_get("", "actor_id")?,
                separation_fingerprint: r.try_get("", "separation_fingerprint")?,
                payload_hash: r.try_get("", "payload_hash")?,
            })
        })
        .collect()
}
