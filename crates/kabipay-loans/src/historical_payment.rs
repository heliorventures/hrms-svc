//! A reviewed late external event retains its occurrence date and adjusts today's ledger.
use crate::{
    access, lifecycle, payments,
    posting::{self, Posting},
    repository as store, HistoricalPaymentKind, LoanActorScope, LoanCommandResult, LoanModuleError,
    LoanResult, RecordedPayment,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalPaymentPreview {
    pub loan_id: Uuid,
    pub account_version: i64,
    pub terms_version_id: Uuid,
    pub occurrence_date: NaiveDate,
    pub posting_date: NaiveDate,
    pub currency: String,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub current_principal: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub current_interest: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub corrected_principal: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub corrected_interest: Decimal,
    pub preserved_payroll_postings: Vec<Uuid>,
    pub review_fingerprint: String,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub(crate) principal_allocation: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub(crate) interest_allocation: Decimal,
    pub(crate) allocations: Vec<serde_json::Value>,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub(crate) carry: Decimal,
}
pub(crate) fn authorize(actor: &LoanActorScope) -> LoanResult<()> {
    access::company(actor)?;
    if !actor.can_correct_company_payments {
        return Err(LoanModuleError::Forbidden);
    }
    Ok(())
}

pub async fn preview_historical_payment(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    kind: HistoricalPaymentKind,
    payment: &RecordedPayment,
) -> LoanResult<HistoricalPaymentPreview> {
    authorize(actor)?;
    if actor.permission != kind.permission() {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    store::synchronize(tx, actor.tenant_id).await?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    let (_, version, terms, _) =
        payments::validate(tx, actor, loan, account.version, payment, true).await?;
    let date = lifecycle::today(tx, actor.tenant_id).await?;
    if !account
        .accrued_through
        .is_some_and(|through| through > payment.value_date && through <= date)
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    if store::one(
        tx,
        "SELECT id FROM loan_unapplied_credit WHERE tenant_id=$1 AND loan_id=$2 LIMIT 1",
        vec![actor.tenant_id.into(), loan.into()],
    )
    .await?
    .is_some()
    {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    if kind == HistoricalPaymentKind::Receipt && payment.resignation_exception_reason.is_some() {
        return Err(LoanModuleError::InvalidCommand);
    }
    if kind == HistoricalPaymentKind::Disbursement {
        if account.state != "OPEN"
            || account.funding_state == "CANCELLED"
            || store::one(
                tx,
                "SELECT id FROM loan_exit_arrangement WHERE tenant_id=$1 AND loan_id=$2",
                vec![actor.tenant_id.into(), loan.into()],
            )
            .await?
            .is_some()
        {
            return Err(LoanModuleError::PolicyUnavailable);
        }
        if posting::net_funding(tx, actor.tenant_id, loan)
            .await?
            .checked_add(payment.amount)
            .ok_or(LoanModuleError::InvalidCommand)?
            > account.approved_principal
        {
            return Err(LoanModuleError::FundingCeilingExceeded);
        }
    }
    let disbursement = kind == HistoricalPaymentKind::Disbursement;
    let replay = crate::replay::calculate(
        tx,
        actor.tenant_id,
        loan,
        None,
        Some(crate::replay::Event {
            id: Uuid::nil(),
            date: payment.value_date,
            sequence: i64::MAX,
            kind: if disbursement {
                "DISBURSEMENT"
            } else {
                "REPAYMENT"
            }
            .into(),
            source: if disbursement {
                "EXTERNAL_DISBURSEMENT"
            } else {
                "EXTERNAL_RECEIPT"
            }
            .into(),
            amount: payment.amount,
            principal: Decimal::ZERO,
            interest: Decimal::ZERO,
            terms,
        }),
        date,
    )
    .await?;
    let (principal_allocation, interest_allocation) = if disbursement {
        (payment.amount, Decimal::ZERO)
    } else {
        replay
            .candidate_allocation
            .ok_or(LoanModuleError::InvalidCommand)?
    };
    let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let mut preview = HistoricalPaymentPreview {
        loan_id: loan,
        account_version: account.version,
        terms_version_id: version.id,
        occurrence_date: payment.value_date,
        posting_date: date,
        currency: account.currency,
        current_principal: principal,
        current_interest: interest,
        corrected_principal: replay.principal,
        corrected_interest: replay.interest,
        preserved_payroll_postings: replay.frozen,
        review_fingerprint: String::new(),
        principal_allocation,
        interest_allocation,
        allocations: replay.allocations,
        carry: replay.carry,
    };
    preview.review_fingerprint =
        crate::command_hash(&(actor.tenant_id, actor.user_id, kind, payment, &preview))?;
    Ok(preview)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    loan: Uuid,
    kind: HistoricalPaymentKind,
    payment: &RecordedPayment,
    reason: &str,
    reference: &str,
    fingerprint: &str,
    context: Option<&kabipay_common::finance_contract::FundingContext>,
) -> LoanResult<LoanCommandResult> {
    lifecycle::text(reason, 2000)?;
    lifecycle::text(reference, 500)?;
    let review = preview_historical_payment(tx, actor, loan, kind, payment).await?;
    lifecycle::checked_version(review.account_version, expected)?;
    if review.review_fingerprint != fingerprint {
        return Err(LoanModuleError::VersionConflict);
    }
    let account = store::account(tx, actor.tenant_id, loan).await?;
    let disbursement = kind == HistoricalPaymentKind::Disbursement;
    if disbursement
        && context.is_some_and(|value| {
            value.employee_id != account.employee_id
                || value.value_date != payment.value_date
                || (value.pending_separation_id.is_some()
                    && payment.resignation_exception_reason.is_none())
        })
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    let id = Uuid::new_v4();
    let evidence = serde_json::json!({"reference":payment.evidence_reference,"resignationExceptionReason":payment.resignation_exception_reason,"fundingContext":context,"classification":"LATE_EXTERNAL_PAYMENT","reason":reason,"reconciliationReference":reference,"reviewFingerprint":fingerprint});
    let sql = if disbursement {
        "INSERT INTO loan_disbursement(id,tenant_id,loan_id,amount,value_date,method,external_reference,evidence,actor_id,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1)"
    } else {
        "INSERT INTO loan_receipt(id,tenant_id,loan_id,amount,value_date,method,external_reference,evidence,actor_id,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1)"
    };
    store::execute(
        tx,
        sql,
        vec![
            id.into(),
            actor.tenant_id.into(),
            loan.into(),
            payment.amount.into(),
            payment.value_date.into(),
            payment.method.clone().into(),
            payment.external_reference.clone().into(),
            evidence.into(),
            actor.user_id.into(),
        ],
    )
    .await?;
    let sign = if disbursement {
        Decimal::ONE
    } else {
        -Decimal::ONE
    };
    let mut ids=vec![posting::post(tx,actor,Posting {loan,terms:review.terms_version_id,kind:if disbursement {"DISBURSEMENT"} else {"REPAYMENT"},source_kind:if disbursement {"EXTERNAL_DISBURSEMENT"} else {"EXTERNAL_RECEIPT"},source:id,revision:1,date:review.posting_date,amount:payment.amount,principal:sign*review.principal_allocation,interest:sign*review.interest_allocation,reversal:None,evidence:serde_json::json!({"classification":"LATE_EXTERNAL_PAYMENT","occurrenceDate":payment.value_date,"reviewFingerprint":fingerprint})}).await?];
    let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let pd = review
        .corrected_principal
        .checked_sub(principal)
        .ok_or(LoanModuleError::InvalidCommand)?;
    let interest_delta = review
        .corrected_interest
        .checked_sub(interest)
        .ok_or(LoanModuleError::InvalidCommand)?;
    if pd != Decimal::ZERO || interest_delta != Decimal::ZERO {
        ids.push(posting::post(tx,actor,Posting {loan,terms:account.current_terms_id.ok_or(LoanModuleError::InvalidCommand)?,kind:"ADJUSTMENT",source_kind:"HISTORICAL_RECONCILIATION",source:id,revision:expected,date:review.posting_date,amount:pd.abs().checked_add(interest_delta.abs()).ok_or(LoanModuleError::InvalidCommand)?,principal:pd,interest:interest_delta,reversal:None,evidence:serde_json::json!({"classification":"LATE_EXTERNAL_PAYMENT_RECONCILIATION","reason":reason,"reconciliationReference":reference,"preview":review})}).await?);
    }
    store::execute(
        tx,
        "UPDATE loan_account SET accrued_through=$3,rounding_carry=$4 WHERE tenant_id=$1 AND id=$2",
        vec![
            actor.tenant_id.into(),
            loan.into(),
            review.posting_date.into(),
            review.carry.into(),
        ],
    )
    .await?;
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    let mut result = lifecycle::result(
        id,
        Some(loan),
        Some(account.employee_id),
        version,
        "RECONCILED",
    );
    result.posting_ids = ids;
    Ok(result)
}
