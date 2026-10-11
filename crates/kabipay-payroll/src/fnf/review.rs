use super::{
    calculation::{payout_pool, SourceKind},
    coordination::financial_error,
    model::{FinancialInput, FinancialSnapshot},
};
use chrono::Datelike;
use kabipay_common::{
    context::{ClientClaims, ScopeType},
    finance_contract::ExitSubject,
    KabiPayError, KabiPayResult,
};
use kabipay_loans::{LoanActorScope, LoanPermission, RecoveryInput, RecoverySource};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter};
use uuid::Uuid;

pub(super) fn actor(claims: &ClientClaims, tenant: Uuid) -> KabiPayResult<LoanActorScope> {
    if kabipay_common::client_data_scope::data_scope_from_claims(Some(claims), "onboarding:manage")?
        != ScopeType::All
    {
        return Err(KabiPayError::Forbidden(
            "FNF financial review requires ALL HR scope".into(),
        ));
    }
    LoanActorScope::from_verified_claims(claims, tenant, LoanPermission::FnfRecovery)
        .map_err(financial_error)
}

async fn verify_sources(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
    input: &FinancialInput,
) -> KabiPayResult<()> {
    use kabipay_db_entities::tenant::d0005_auth_rbac::user;
    use kabipay_db_entities::tenant::d0012_payroll::{payroll_cycle, payslip};
    for decision in &input.loan_decision.loans {
        if let Some(continuing) = &decision.continuing {
            let owner = user::Entity::find_by_id(continuing.follow_up_user_id)
                .filter(user::Column::TenantId.eq(tenant))
                .filter(user::Column::IsActive.eq(true))
                .filter(user::Column::IsDeleted.eq(false))
                .one(tx)
                .await?;
            if owner.is_none() {
                return Err(KabiPayError::Validation(
                    "Repayment follow-up owner must be an active company user".into(),
                ));
            }
        }
    }
    for source in &input.sources {
        if source.kind != SourceKind::PayrollReference {
            continue;
        }
        let slip = payslip::Entity::find_by_id(source.source_id)
            .filter(payslip::Column::TenantId.eq(tenant))
            .filter(payslip::Column::EmployeeId.eq(employee))
            .one(tx)
            .await?
            .ok_or_else(|| {
                KabiPayError::Validation(
                    "Salary references must identify this employee's issued payslip".into(),
                )
            })?;
        let cycle = payroll_cycle::Entity::find_by_id(slip.payroll_cycle_id)
            .filter(payroll_cycle::Column::TenantId.eq(tenant))
            .one(tx)
            .await?
            .ok_or_else(|| {
                KabiPayError::Validation("Referenced payroll cycle is unavailable".into())
            })?;
        if cycle.status != "PROCESSED" || source.amount != slip.net_salary {
            return Err(KabiPayError::Validation("Verify the issued salary amount; payroll references cannot become new FNF earnings".into()));
        }
    }
    Ok(())
}

pub(super) async fn calculate(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    claims: &ClientClaims,
    subject: &ExitSubject,
    fnf: Uuid,
    revision: i64,
    input: &FinancialInput,
) -> KabiPayResult<FinancialSnapshot> {
    if input.separation_id != subject.separation_id
        || subject.status != "APPROVED"
        || input.financial_approval_reference.trim().is_empty()
        || input.financial_approval_reference.len() > 500
    {
        return Err(KabiPayError::Validation(
            "FNF requires approved separation and reviewed financial inputs".into(),
        ));
    }
    let actor = actor(claims, tenant)?;
    let state = kabipay_loans::payroll_loan_state(tx, tenant)
        .await
        .map_err(financial_error)?;
    if state.currency.as_deref() != Some(input.currency.code.as_str()) {
        return Err(KabiPayError::Validation(
            "FNF currency must match the configured company currency".into(),
        ));
    }
    let pool = payout_pool(&input.currency, &input.sources)?;
    verify_sources(tx, tenant, subject.employee_id, input).await?;
    let exposure =
        kabipay_loans::preview_exit_exposure(tx, &actor, subject.employee_id, input.value_date)
            .await
            .map_err(financial_error)?;
    if exposure
        .loans
        .iter()
        .any(|line| line.currency != input.currency)
    {
        return Err(KabiPayError::Validation(
            "All loans in this FNF must use its currency and precision".into(),
        ));
    }
    if input.loan_decision.loans.iter().any(|line| {
        line.proposed_payroll_recovery != Decimal::ZERO || line.final_payroll_cycle_id.is_some()
    }) {
        return Err(KabiPayError::Validation(
            "Finalize salary recovery in Payroll before reviewing actual FNF recovery".into(),
        ));
    }
    kabipay_loans::validate_exit_decision(&exposure, &input.loan_decision)
        .map_err(financial_error)?;
    if let Some(id) = input.exit_review_id {
        let review = kabipay_loans::find_exit_review(tx, &actor, id)
            .await
            .map_err(financial_error)?;
        if review.separation_id != subject.separation_id
            || review.exposure.employee_id != subject.employee_id
        {
            return Err(KabiPayError::Forbidden(
                "Exit review belongs to a different separation".into(),
            ));
        }
    }
    let quote = if state.enabled {
        let limits = input
            .loan_decision
            .loans
            .iter()
            .map(|line| (line.loan_id, line.proposed_fnf_recovery))
            .collect();
        Some(
            kabipay_loans::prepare_fnf_recovery_quote(
                tx,
                &actor,
                &RecoveryInput {
                    employee_id: subject.employee_id,
                    source: RecoverySource::Fnf,
                    source_id: fnf,
                    source_revision: revision,
                    period_start: input.value_date.with_day(1).ok_or_else(|| {
                        KabiPayError::Validation("Invalid settlement value date".into())
                    })?,
                    value_date: input.value_date,
                    eligible_net: pool.eligible_for_loan,
                    available_net: pool.eligible_for_loan,
                    currency: input.currency.clone(),
                },
                &limits,
            )
            .await
            .map_err(financial_error)?,
        )
    } else {
        None
    };
    let mut actual = input.loan_decision.clone();
    for line in &mut actual.loans {
        line.proposed_fnf_recovery = quote
            .as_ref()
            .and_then(|quote| {
                quote
                    .lines()
                    .iter()
                    .find(|recovery| recovery.loan_id == line.loan_id)
            })
            .map_or(Decimal::ZERO, |recovery| {
                recovery.principal + recovery.interest
            });
    }
    // A payout shortfall cannot erase the residual or bypass its repayment arrangement.
    kabipay_loans::validate_exit_decision(&exposure, &actual).map_err(financial_error)?;
    let fingerprint = kabipay_loans::payroll_loan_state(tx, tenant)
        .await
        .map_err(financial_error)?
        .fingerprint;
    FinancialSnapshot::from_pool(
        input.clone(),
        subject.fingerprint.clone(),
        fingerprint,
        exposure,
        quote,
        pool,
    )
}
