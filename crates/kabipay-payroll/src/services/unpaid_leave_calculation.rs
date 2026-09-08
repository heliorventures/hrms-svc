//! Monetary calculation independent of leave eligibility and payroll treatment.

use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;

pub struct PayRunAmounts {
    pub gross: Decimal,
    pub total_deductions: Decimal,
    pub net: Decimal,
    pub earning_reduction: Decimal,
    pub separate_deduction: Decimal,
    pub statutory: super::statutory_india::IndiaStatutoryStub,
    pub tds: Decimal,
}

pub fn apply_payroll_treatment(original_gross: Decimal, other_deductions: Decimal, tds: Option<Decimal>, amount: Decimal, treatment: Option<&str>) -> KabiPayResult<PayRunAmounts> {
    let before = treatment == Some(super::unpaid_leave_policy::BEFORE);
    if amount < Decimal::ZERO || amount > original_gross || (amount > Decimal::ZERO && treatment != Some(super::unpaid_leave_policy::BEFORE) && treatment != Some(super::unpaid_leave_policy::AFTER)) {
        return Err(KabiPayError::Validation("invalid unpaid leave payroll treatment or amount".into()));
    }
    let earning_reduction = if before { amount } else { Decimal::ZERO };
    let separate_deduction = if before { Decimal::ZERO } else { amount };
    let gross = (original_gross - earning_reduction).round_dp(2);
    let (statutory, tds) = super::statutory_india::compute(gross, tds);
    let total_deductions = (other_deductions + separate_deduction + super::statutory_india::employee_deduction_total(&statutory, tds)).round_dp(2);
    let net = (gross - total_deductions).round_dp(2);
    if amount > Decimal::ZERO && net < Decimal::ZERO {
        return Err(KabiPayError::Validation("unpaid leave would produce negative net salary; review the employee's deductions before running payroll".into()));
    }
    Ok(PayRunAmounts { gross, total_deductions, net, earning_reduction, separate_deduction, statutory, tds })
}

pub fn calculate_unpaid_leave(
    basic: Decimal,
    divisor: Decimal,
    unpaid_days: Decimal,
) -> KabiPayResult<Decimal> {
    if divisor <= Decimal::ZERO || basic < Decimal::ZERO || unpaid_days < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "unpaid leave requires a positive divisor and non-negative basic salary and days".into(),
        ));
    }
    if basic.is_zero() || unpaid_days.is_zero() {
        return Ok(Decimal::ZERO);
    }
    basic
        .checked_div(divisor)
        .and_then(|daily_rate| daily_rate.checked_mul(unpaid_days))
        .map(|amount| amount.round_dp(2))
        .ok_or_else(|| KabiPayError::Validation("unpaid leave calculation exceeds supported amount".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_statutory_reduces_earnings_once_and_reconciles_net() {
        let result = apply_payroll_treatment(10000.into(), Decimal::ZERO, None, 1000.into(), Some("BEFORE_STATUTORY")).unwrap();
        assert_eq!(result.gross, Decimal::from(9000));
        assert_eq!(result.earning_reduction, Decimal::from(1000));
        assert_eq!(result.separate_deduction, Decimal::ZERO);
        assert_eq!(result.statutory.pf_employee, Decimal::from(1080));
        assert_eq!(result.total_deductions, Decimal::new(114750, 2));
        assert_eq!(result.net, Decimal::new(785250, 2));
    }

    #[test]
    fn after_statutory_preserves_earnings_and_adds_one_deduction() {
        let result = apply_payroll_treatment(10000.into(), Decimal::ZERO, None, 1000.into(), Some("AFTER_STATUTORY")).unwrap();
        assert_eq!(result.gross, Decimal::from(10000));
        assert_eq!(result.earning_reduction, Decimal::ZERO);
        assert_eq!(result.separate_deduction, Decimal::from(1000));
        assert_eq!(result.statutory.pf_employee, Decimal::from(1200));
        assert_eq!(result.total_deductions, Decimal::from(2275));
        assert_eq!(result.net, Decimal::from(7725));
    }

    #[test]
    fn no_deduction_retains_existing_payroll_and_tds_input() {
        let result = apply_payroll_treatment(10000.into(), 100.into(), Some(500.into()), Decimal::ZERO, None).unwrap();
        assert_eq!(result.gross, Decimal::from(10000));
        assert_eq!(result.net, Decimal::from(8125));
        assert_eq!(result.tds, Decimal::from(500));
    }

    #[test]
    fn invalid_treatment_and_negative_net_are_rejected() {
        assert!(apply_payroll_treatment(1000.into(), Decimal::ZERO, None, 100.into(), Some("INVALID")).is_err());
        assert!(apply_payroll_treatment(1000.into(), 1000.into(), None, 100.into(), Some("AFTER_STATUTORY")).is_err());
    }

    #[test]
    fn uses_the_configured_divisor_and_half_days() {
        assert_eq!(
            calculate_unpaid_leave(Decimal::from(30000), Decimal::from(30), Decimal::new(15, 1)).unwrap(),
            Decimal::from(1500)
        );
        assert_eq!(
            calculate_unpaid_leave(Decimal::from(26000), Decimal::from(26), Decimal::ONE).unwrap(),
            Decimal::from(1000)
        );
    }

    #[test]
    fn rounds_the_final_amount_not_the_daily_rate() {
        assert_eq!(
            calculate_unpaid_leave(Decimal::from(10000), Decimal::from(30), Decimal::from(3)).unwrap(),
            Decimal::from(1000)
        );
    }

    #[test]
    fn no_unpaid_leave_has_no_deduction() {
        assert_eq!(
            calculate_unpaid_leave(Decimal::from(30000), Decimal::from(30), Decimal::ZERO).unwrap(),
            Decimal::ZERO
        );
    }

    #[test]
    fn rejects_invalid_configuration_and_negative_inputs() {
        for (basic, divisor, days) in [(100, 0, 1), (100, -1, 1), (-1, 30, 1), (100, 30, -1)] {
            assert!(calculate_unpaid_leave(basic.into(), divisor.into(), days.into()).is_err());
        }
    }

    #[test]
    fn overflow_is_an_error_not_a_panic_or_wrapped_amount() {
        assert!(calculate_unpaid_leave(Decimal::MAX, Decimal::ONE, Decimal::from(2)).is_err());
    }
}
