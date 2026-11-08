//! Reviewed payout sources are independent of Loan debt and Payroll salary references.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_loans_domain::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceKind {
    LeaveEncashment,
    Gratuity,
    Bonus,
    PayrollReference,
}
impl SourceKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::LeaveEncashment => "LEAVE_ENCASHMENT",
            Self::Gratuity => "GRATUITY",
            Self::Bonus => "BONUS",
            Self::PayrollReference => "PAYROLL_REFERENCE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedPayoutSource {
    pub kind: SourceKind,
    pub source_id: Uuid,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub amount: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub already_paid: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub other_recovery: Decimal,
    pub eligible_for_loan: bool,
    pub approval_reference: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutPool {
    pub available: Decimal,
    pub eligible_for_loan: Decimal,
    pub other_recoveries: Decimal,
}

fn invalid() -> KabiPayError {
    KabiPayError::Validation(
        "Review complete FNF sources, approvals, paid amounts and recovery allocations".into(),
    )
}
fn add(left: Decimal, right: Decimal) -> KabiPayResult<Decimal> {
    left.checked_add(right).ok_or_else(invalid)
}

pub fn payout_pool(
    currency: &Currency,
    sources: &[ReviewedPayoutSource],
) -> KabiPayResult<PayoutPool> {
    if sources.len() > 100 {
        return Err(invalid());
    }
    let mut seen = std::collections::HashSet::new();
    let mut pool = PayoutPool {
        available: Decimal::ZERO,
        eligible_for_loan: Decimal::ZERO,
        other_recoveries: Decimal::ZERO,
    };
    for source in sources {
        if source.source_id.is_nil()
            || !seen.insert((source.kind.wire(), source.source_id))
            || source.approval_reference.trim().is_empty()
            || source.approval_reference.len() > 500
        {
            return Err(invalid());
        }
        for amount in [source.amount, source.already_paid, source.other_recovery] {
            currency.validate_amount(amount).map_err(|_| invalid())?;
        }
        let remaining = source
            .amount
            .checked_sub(add(source.already_paid, source.other_recovery)?)
            .ok_or_else(invalid)?;
        if remaining < Decimal::ZERO {
            return Err(invalid());
        }
        if source.kind == SourceKind::PayrollReference {
            if source.eligible_for_loan || source.other_recovery != Decimal::ZERO {
                return Err(invalid());
            }
            continue;
        }
        pool.available = add(pool.available, remaining)?;
        pool.other_recoveries = add(pool.other_recoveries, source.other_recovery)?;
        if source.eligible_for_loan {
            pool.eligible_for_loan = add(pool.eligible_for_loan, remaining)?;
        }
    }
    // Explicit approved zero-value sources distinguish resolved components from missing calculations.
    for kind in [
        SourceKind::LeaveEncashment,
        SourceKind::Gratuity,
        SourceKind::Bonus,
    ] {
        if !sources.iter().any(|source| source.kind == kind) {
            return Err(invalid());
        }
        let total = sources
            .iter()
            .filter(|source| source.kind == kind)
            .try_fold(Decimal::ZERO, |total, source| {
                total.checked_add(source.amount)
            })
            .ok_or_else(invalid)?;
        currency.validate_amount(total).map_err(|_| invalid())?;
    }
    for amount in [
        pool.available,
        pool.eligible_for_loan,
        pool.other_recoveries,
    ] {
        currency.validate_amount(amount).map_err(|_| invalid())?;
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(kind: SourceKind, amount: i64, eligible: bool) -> ReviewedPayoutSource {
        ReviewedPayoutSource {
            kind,
            source_id: Uuid::new_v4(),
            amount: amount.into(),
            already_paid: Decimal::ZERO,
            other_recovery: Decimal::ZERO,
            eligible_for_loan: eligible,
            approval_reference: "Approved benefit valuation and set-off review".into(),
        }
    }
    fn currency() -> Currency {
        Currency {
            code: "INR".into(),
            minor_units: 2,
        }
    }
    #[test]
    fn references_paid_sources_and_noneligible_benefits_never_inflate_loan_capacity() {
        let mut sources = vec![
            source(SourceKind::LeaveEncashment, 8000, true),
            source(SourceKind::Gratuity, 4000, false),
            source(SourceKind::Bonus, 0, true),
            source(SourceKind::PayrollReference, 20000, false),
        ];
        sources[0].already_paid = 1000.into();
        sources[0].other_recovery = 2000.into();
        let pool = payout_pool(&currency(), &sources).unwrap();
        assert_eq!(pool.available, 9000.into());
        assert_eq!(pool.eligible_for_loan, 5000.into());
        assert_eq!(pool.other_recoveries, 2000.into());
        sources[3].eligible_for_loan = true;
        assert!(payout_pool(&currency(), &sources).is_err());
    }
    #[test]
    fn incomplete_duplicate_negative_or_overallocated_sources_require_review() {
        let mut sources = vec![
            source(SourceKind::LeaveEncashment, 1000, true),
            source(SourceKind::Gratuity, 0, false),
            source(SourceKind::Bonus, 0, false),
        ];
        assert!(payout_pool(&currency(), &sources[..2]).is_err());
        sources.push(sources[0].clone());
        assert!(payout_pool(&currency(), &sources).is_err());
        sources.pop();
        sources[0].already_paid = 800.into();
        sources[0].other_recovery = 300.into();
        assert!(payout_pool(&currency(), &sources).is_err());
        sources[0].other_recovery = (-1).into();
        assert!(payout_pool(&currency(), &sources).is_err());
        sources[0].other_recovery = Decimal::new(1, 3);
        assert!(payout_pool(&currency(), &sources).is_err());
    }
}
