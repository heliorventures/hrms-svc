use chrono::NaiveDate;
use kabipay_payroll::services::imported_payroll;

#[test]
fn joining_midmonth_still_requires_reviewed_inputs_for_that_month() {
    let cycle = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    let joining = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
    assert!(joining <= imported_payroll::rule_cutoff(cycle).unwrap());
    let next_month_joining = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    assert!(next_month_joining > imported_payroll::rule_cutoff(cycle).unwrap());
}
