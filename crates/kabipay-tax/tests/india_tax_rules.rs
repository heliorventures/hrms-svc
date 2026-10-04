use kabipay_tax::domain::{
    income_tax::{calculate_income_tax, IncomeTaxInput},
    TaxRegime,
};
use rust_decimal::Decimal;
fn input(gross: i64, regime: TaxRegime) -> IncomeTaxInput {
    IncomeTaxInput {
        fiscal_year: 2026,
        regime,
        gross: Decimal::from(gross),
        approved_deductions: Decimal::ZERO,
        resident: Some(true),
        age_at_year_end: Some(40),
        special_rate_income: false,
    }
}
#[test]
fn example_projection_and_statutory_rounding_are_distinct() {
    let result = calculate_income_tax(&input(3466548, TaxRegime::New)).unwrap();
    assert_eq!(result.display_tax, Decimal::from(621363));
    assert_eq!(result.statutory_tax, Decimal::from(621360));
    assert_eq!(result.standard_deduction, Decimal::from(75000));
}
#[test]
fn new_rebate_and_marginal_relief_boundaries() {
    assert_eq!(
        calculate_income_tax(&input(1275000, TaxRegime::New))
            .unwrap()
            .statutory_tax,
        Decimal::ZERO
    );
    assert_eq!(
        calculate_income_tax(&input(1285000, TaxRegime::New))
            .unwrap()
            .statutory_tax,
        Decimal::from(10400)
    );
}
#[test]
fn old_regime_and_unverified_residency() {
    assert_eq!(
        calculate_income_tax(&input(550000, TaxRegime::Old))
            .unwrap()
            .statutory_tax,
        Decimal::ZERO
    );
    let mut value = input(1000000, TaxRegime::New);
    value.resident = None;
    assert!(calculate_income_tax(&value).is_err());
    value.resident = Some(true);
    value.special_rate_income = true;
    assert!(calculate_income_tax(&value).is_err());
}
#[test]
fn deductions_do_not_reduce_new_regime_tax_automatically() {
    let base = input(2000000, TaxRegime::New);
    let mut declared = base.clone();
    declared.approved_deductions = Decimal::from(150000);
    assert_eq!(
        calculate_income_tax(&base).unwrap().statutory_tax,
        calculate_income_tax(&declared).unwrap().statutory_tax
    );
}
