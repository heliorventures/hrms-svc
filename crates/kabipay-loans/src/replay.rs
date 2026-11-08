//! Replay occurrence dates; issued allocation splits remain immutable source evidence.
use crate::{accrual, repository as store, LoanModuleError, LoanResult};
use chrono::NaiveDate;
use kabipay_loans_domain::{
    accrue_interest, allocate_repayment, AccrualInput, AllocationInput, InterestMethod, LoanTerms,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) struct Event {
    pub id: Uuid,
    pub date: NaiveDate,
    pub sequence: i64,
    pub kind: String,
    pub source: String,
    pub amount: Decimal,
    pub principal: Decimal,
    pub interest: Decimal,
    pub terms: LoanTerms,
}
pub(crate) struct Replay {
    pub principal: Decimal,
    pub interest: Decimal,
    pub carry: Decimal,
    pub frozen: Vec<Uuid>,
    pub allocations: Vec<serde_json::Value>,
    pub candidate_allocation: Option<(Decimal, Decimal)>,
}
fn add(left: Decimal, right: Decimal) -> LoanResult<Decimal> {
    left.checked_add(right)
        .ok_or(LoanModuleError::InvalidCommand)
}

pub(crate) async fn calculate(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
    removed: Option<Uuid>,
    candidate: Option<Event>,
    date: NaiveDate,
) -> LoanResult<Replay> {
    let account = store::account(tx, tenant, loan).await?;
    let rows = store::all(tx, r#"SELECT p.id,p.kind,p.source_kind,COALESCE(d.value_date,r.value_date,p.value_date) AS occurrence_date,l.sequence,
        p.amount::text AS amount,p.principal_delta::text AS principal,p.interest_delta::text AS interest,t.terms
        FROM loan_posting p JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id
        JOIN loan_terms_version t ON t.tenant_id=p.tenant_id AND t.loan_id=p.loan_id AND t.id=p.terms_version_id
        LEFT JOIN loan_disbursement d ON d.tenant_id=p.tenant_id AND d.loan_id=p.loan_id AND d.id=p.source_id AND p.source_kind='EXTERNAL_DISBURSEMENT'
        LEFT JOIN loan_receipt r ON r.tenant_id=p.tenant_id AND r.loan_id=p.loan_id AND r.id=p.source_id AND p.source_kind='EXTERNAL_RECEIPT'
        WHERE p.tenant_id=$1 AND p.loan_id=$2 AND ($3::uuid IS NULL OR p.id<>$3)
        AND p.kind IN ('DISBURSEMENT','REPAYMENT') AND NOT EXISTS(SELECT 1 FROM loan_posting v WHERE v.tenant_id=p.tenant_id AND v.loan_id=p.loan_id AND v.reversal_of=p.id)"#,
        vec![tenant.into(),loan.into(),removed.into()]).await?;
    let mut events = rows
        .into_iter()
        .map(|row| {
            Ok(Event {
                id: row.try_get("", "id")?,
                date: row.try_get("", "occurrence_date")?,
                sequence: row.try_get("", "sequence")?,
                kind: row.try_get("", "kind")?,
                source: row.try_get("", "source_kind")?,
                amount: store::decimal(&row, "amount")?,
                principal: store::decimal(&row, "principal")?,
                interest: store::decimal(&row, "interest")?,
                terms: serde_json::from_value(row.try_get("", "terms")?)?,
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    if let Some(candidate) = candidate {
        events.push(candidate);
    }
    events.sort_by_key(|event| (event.date, event.sequence));
    let mut result = Replay {
        principal: Decimal::ZERO,
        interest: Decimal::ZERO,
        carry: Decimal::ZERO,
        frozen: vec![],
        allocations: vec![],
        candidate_allocation: None,
    };
    let mut cursor = None;
    for event in events {
        event.terms.validate()?;
        if event.date > date
            || event.terms.currency.code != account.currency
            || i32::try_from(event.terms.currency.minor_units).ok() != Some(account.minor_units)
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        let earned = accrual::calculate(
            tx,
            tenant,
            loan,
            &event.terms,
            result.principal,
            cursor,
            event.date,
            result.carry,
        )
        .await?;
        result.interest = add(result.interest, earned.amount)?;
        result.carry = earned.carry;
        if event.kind == "DISBURSEMENT" {
            result.principal = add(result.principal, event.amount)?;
            if matches!(
                event.terms.interest,
                InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
            ) {
                let charge = accrue_interest(&AccrualInput {
                    principal: event.amount,
                    method: event.terms.interest.clone(),
                    start: event.date,
                    end_exclusive: event.date,
                    carry: result.carry,
                    currency: event.terms.currency.clone(),
                    rounding: event.terms.rounding,
                })?;
                result.interest = add(result.interest, charge.posted_interest)?;
                result.carry = charge.carry;
            }
        } else if matches!(
            event.source.as_str(),
            "PAYROLL" | "FNF" | "EXTERNAL_RECEIPT"
        ) {
            if matches!(event.source.as_str(), "PAYROLL" | "FNF") {
                result.frozen.push(event.id);
            }
            let allocation = allocate_repayment(&AllocationInput {
                payment: event.amount,
                principal: result.principal,
                interest: result.interest,
                order: event.terms.allocation,
                currency: event.terms.currency,
            })?;
            if allocation.excess_credit > Decimal::ZERO {
                return Err(LoanModuleError::HistoricalReconciliationRequired);
            }
            if event.id.is_nil() {
                result.candidate_allocation = Some((allocation.principal, allocation.interest));
            }
            result.allocations.push(serde_json::json!({"postingId":event.id,"sourceKind":event.source,"originalPrincipal":event.principal.to_string(),"originalInterest":event.interest.to_string(),"correctedPrincipal":(-allocation.principal).to_string(),"correctedInterest":(-allocation.interest).to_string()}));
            result.principal = add(result.principal, -allocation.principal)?;
            result.interest = add(result.interest, -allocation.interest)?;
        } else {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        if result.principal < Decimal::ZERO || result.interest < Decimal::ZERO {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        cursor = Some(event.date);
    }
    let version = store::terms(tx, &account).await?;
    let terms: LoanTerms = serde_json::from_value(version.terms)?;
    let earned = accrual::calculate(
        tx,
        tenant,
        loan,
        &terms,
        result.principal,
        cursor,
        date,
        result.carry,
    )
    .await?;
    result.interest = add(result.interest, earned.amount)?;
    result.carry = earned.carry;
    Ok(result)
}
