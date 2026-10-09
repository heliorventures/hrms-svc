use crate::{
    access, accrual, lifecycle,
    posting::{self, Posting},
    repository as store, LoanActorScope, LoanCommandResult, LoanModuleError, LoanResult,
    RecordedPayment,
};
use kabipay_loans_domain::{
    accrue_interest, allocate_repayment, AccrualInput, AllocationInput, ExcessCreditPolicy,
    InterestMethod, LoanPolicy, LoanTerms,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

async fn validate(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    expected: i64,
    payment: &RecordedPayment,
) -> LoanResult<(
    kabipay_db_entities::tenant::d0103_employee_loans::loan_account::Model,
    kabipay_db_entities::tenant::d0103_employee_loans::loan_terms_version::Model,
    LoanTerms,
    LoanPolicy,
)> {
    let account = store::account(tx, actor.tenant_id, loan).await?;
    lifecycle::checked_version(account.version, expected)?;
    access::employee(tx, actor, account.employee_id).await?;
    let version = store::terms(tx, &account).await?;
    let terms: LoanTerms = serde_json::from_value(version.terms.clone())?;
    terms.validate()?;
    let policy = store::policy(tx, actor.tenant_id, version.policy_version_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate_terms(&terms)?;
    lifecycle::text(&payment.method, 32)?;
    lifecycle::text(&payment.external_reference, 128)?;
    lifecycle::text(&payment.evidence_reference, 500)?;
    terms.currency.validate_amount(payment.amount)?;
    if payment.amount <= Decimal::ZERO
        || payment.value_date < version.effective_from
        || payment.value_date > lifecycle::today(tx, actor.tenant_id).await?
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    if account
        .accrued_through
        .is_some_and(|date| date > payment.value_date)
    {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    Ok((account, version, terms, rules))
}
pub(crate) async fn disburse(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    expected: i64,
    payment: &RecordedPayment,
) -> LoanResult<LoanCommandResult> {
    let (account, version, terms, _rules) = validate(tx, actor, loan, expected, payment).await?;
    if account.state != "OPEN" || account.funding_state == "CANCELLED" {
        return Err(LoanModuleError::InvalidCommand);
    }
    let funding = posting::net_funding(tx, actor.tenant_id, loan).await?;
    if funding
        .checked_add(payment.amount)
        .ok_or(LoanModuleError::InvalidCommand)?
        > account.approved_principal
    {
        return Err(LoanModuleError::FundingCeilingExceeded);
    }
    let id = Uuid::new_v4();
    let mut ids = accrual::accrue(
        tx,
        actor,
        loan,
        version.id,
        &terms,
        payment.value_date,
        id,
        expected,
    )
    .await?;
    // This is the carry after earned accrual, before this funding tranche.
    // Capture it even when the proportional charge rounds to zero.
    let fresh = store::account(tx, actor.tenant_id, loan).await?;
    store::execute(tx,"INSERT INTO loan_disbursement(id,tenant_id,loan_id,amount,value_date,method,external_reference,evidence,actor_id,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1)",vec![id.into(),actor.tenant_id.into(),loan.into(),payment.amount.into(),payment.value_date.into(),payment.method.clone().into(),payment.external_reference.clone().into(),serde_json::json!({"reference":payment.evidence_reference}).into(),actor.user_id.into()]).await?;
    ids.push(posting::post(tx,actor,Posting{loan,terms:version.id,kind:"DISBURSEMENT",source_kind:"EXTERNAL_DISBURSEMENT",source:id,revision:1,date:payment.value_date,amount:payment.amount,principal:payment.amount,interest:Decimal::ZERO,reversal:None,evidence:serde_json::json!({"reference":payment.external_reference,"calculatorVersion":terms.calculator_version,"carryBefore":fresh.rounding_carry})}).await?);
    if matches!(
        terms.interest,
        InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
    ) {
        let charge = accrue_interest(&AccrualInput {
            principal: payment.amount,
            method: terms.interest.clone(),
            start: payment.value_date,
            end_exclusive: payment.value_date,
            carry: fresh.rounding_carry,
            currency: terms.currency.clone(),
            rounding: terms.rounding,
        })?;
        if charge.posted_interest > Decimal::ZERO {
            ids.push(posting::post(tx,actor,Posting{loan,terms:version.id,kind:"INTEREST",source_kind:"FUNDING_CHARGE",source:id,revision:1,date:payment.value_date,amount:charge.posted_interest,principal:Decimal::ZERO,interest:charge.posted_interest,reversal:None,evidence:serde_json::json!({"tranchePrincipal":payment.amount,"carryBefore":fresh.rounding_carry,"carryAfter":charge.carry,"calculatorVersion":terms.calculator_version})}).await?);
        }
        store::execute(
            tx,
            "UPDATE loan_account SET rounding_carry=$3 WHERE tenant_id=$1 AND id=$2",
            vec![actor.tenant_id.into(), loan.into(), charge.carry.into()],
        )
        .await?;
    }
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    let mut result = lifecycle::result(
        id,
        Some(loan),
        Some(account.employee_id),
        version,
        "RECORDED",
    );
    result.posting_ids = ids;
    Ok(result)
}
pub(crate) async fn receipt(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    expected: i64,
    payment: &RecordedPayment,
) -> LoanResult<LoanCommandResult> {
    let (account, version, terms, rules) = validate(tx, actor, loan, expected, payment).await?;
    let id = Uuid::new_v4();
    let mut ids = accrual::accrue(
        tx,
        actor,
        loan,
        version.id,
        &terms,
        payment.value_date,
        id,
        expected,
    )
    .await?;
    let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let allocation = allocate_repayment(&AllocationInput {
        payment: payment.amount,
        principal,
        interest,
        order: terms.allocation,
        currency: terms.currency.clone(),
    })?;
    if allocation.excess_credit > Decimal::ZERO
        && matches!(rules.excess_credit, ExcessCreditPolicy::Reject)
    {
        return Err(LoanModuleError::ExcessPayment);
    }
    store::execute(tx,"INSERT INTO loan_receipt(id,tenant_id,loan_id,amount,value_date,method,external_reference,evidence,actor_id,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1)",vec![id.into(),actor.tenant_id.into(),loan.into(),payment.amount.into(),payment.value_date.into(),payment.method.clone().into(),payment.external_reference.clone().into(),serde_json::json!({"reference":payment.evidence_reference}).into(),actor.user_id.into()]).await?;
    let recovered = allocation
        .principal
        .checked_add(allocation.interest)
        .ok_or(LoanModuleError::InvalidCommand)?;
    if recovered > Decimal::ZERO {
        ids.push(posting::post(tx,actor,Posting{loan,terms:version.id,kind:"REPAYMENT",source_kind:"EXTERNAL_RECEIPT",source:id,revision:1,date:payment.value_date,amount:recovered,principal:-allocation.principal,interest:-allocation.interest,reversal:None,evidence:serde_json::json!({"reference":payment.external_reference,"allocationOrder":terms.allocation,"excessCredit":allocation.excess_credit,"calculatorVersion":terms.calculator_version})}).await?);
    }
    if allocation.excess_credit > Decimal::ZERO {
        store::execute(tx,"INSERT INTO loan_unapplied_credit(id,tenant_id,loan_id,receipt_id,amount,remaining_amount,version) VALUES($1,$2,$3,$4,$5,$5,1)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),loan.into(),id.into(),allocation.excess_credit.into()]).await?;
    }
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    let mut result = lifecycle::result(
        id,
        Some(loan),
        Some(account.employee_id),
        version,
        "RECORDED",
    );
    result.posting_ids = ids;
    result.unapplied_credit = allocation.excess_credit;
    Ok(result)
}
