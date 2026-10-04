use kabipay_payroll::services::contribution_calculation::{calculate_esi, calculate_esi_wages};
use kabipay_payroll::services::contribution_rules::{EsiInput, WageClassification};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
#[test]
fn statutory_wages_are_not_universally_half_of_gross() {
    let wages = BTreeMap::from([
        ("BASIC".into(), Decimal::from(16000)),
        ("DA".into(), Decimal::from(2000)),
        ("HRA".into(), Decimal::from(8000)),
    ]);
    let rules = BTreeMap::from([
        ("BASIC".into(), WageClassification::Included),
        ("DA".into(), WageClassification::Included),
        ("HRA".into(), WageClassification::ExcludedWithAddback),
    ]);
    assert_eq!(
        calculate_esi_wages(&wages, &rules).unwrap(),
        Decimal::from(18000)
    );
    let low_basic = BTreeMap::from([
        ("BASIC".into(), Decimal::from(5000)),
        ("HRA".into(), Decimal::from(15000)),
    ]);
    assert_eq!(
        calculate_esi_wages(&low_basic, &rules).unwrap(),
        Decimal::from(10000)
    );
    assert!(calculate_esi_wages(&wages, &BTreeMap::new()).is_err());
}
fn input() -> EsiInput {
    EsiInput {
        as_of: "2026-10-01".parse().unwrap(),
        company_covered: true,
        employee_eligible: Some(true),
        regular_wages: Decimal::from(20000),
        earned_wages: Decimal::from(18000),
        continuation_until: None,
        disability: Some(false),
        average_daily_wage: Some(Decimal::from(600)),
    }
}
#[test]
fn rates_continuation_and_daily_wage_exemption_are_separate() {
    let mut value = input();
    let result = calculate_esi(&value).unwrap();
    assert_eq!(result.employee, Decimal::from(135));
    assert_eq!(result.employer, Decimal::from(585));
    value.regular_wages = Decimal::from(22000);
    assert!(calculate_esi(&value).is_err());
    value.continuation_until = Some("2027-03-31".parse().unwrap());
    assert_eq!(calculate_esi(&value).unwrap().employee, Decimal::from(135));
    value.average_daily_wage = Some(Decimal::from(176));
    assert_eq!(calculate_esi(&value).unwrap().employee, Decimal::ZERO);
    assert_eq!(calculate_esi(&value).unwrap().employer, Decimal::from(585));
    value.employee_eligible = None;
    assert!(calculate_esi(&value).is_err());
}
