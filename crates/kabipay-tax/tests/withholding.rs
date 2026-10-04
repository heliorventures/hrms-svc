use kabipay_tax::domain::withholding::allocate;
use rust_decimal::Decimal;
#[test]
fn verified_history_is_subtracted_once_over_remaining_months() {
    let result = allocate(Decimal::from(621363), Some(Decimal::from(252400)), 12, 7).unwrap();
    assert_eq!(result.monthly, Decimal::from(52709));
    assert_eq!(result.remaining, Some(Decimal::from(368963)));
    assert!(!result.requires_acknowledgement);
}
#[test]
fn unknown_history_stays_unknown_and_uses_employment_months() {
    let result = allocate(Decimal::from(110000), None, 11, 6).unwrap();
    assert_eq!(result.monthly, Decimal::from(10000));
    assert_eq!(result.remaining, None);
    assert!(result.requires_acknowledgement);
}
#[test]
fn excess_withholding_requires_review_and_never_fabricates_a_refund() {
    let result = allocate(Decimal::from(100), Some(Decimal::from(200)), 12, 1).unwrap();
    assert_eq!(result.monthly, Decimal::ZERO);
    assert!(result.excess_withholding);
    assert!(allocate(Decimal::from(100), None, 0, 0).is_err());
}
