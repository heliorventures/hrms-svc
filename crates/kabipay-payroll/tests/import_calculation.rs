use kabipay_payroll::services::payroll_rules::{calculate_period, PeriodInput};
use serde_json::json;

fn input() -> PeriodInput {
    serde_json::from_value(json!({
        "year":2026,"month":9,"gross_rule":"FIXED_MINUS_LWP","fixed_gross":"30000",
        "month_days":"30","paid_days":"27","lwp_days":"3","lwp_divisor":"31","lwp_basis":"GROSS",
        "lwp_amount_override":"2903.23","lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION",
        "variable_allowance_ot":"0","incentive":"500","advance_already_paid":"5000",
        "additional_deductions":[],"statutory_overrides":{"PF":"2400","ESI":"0","PT":"200","TDS":"100"},
        "expected_earned_components":{"BASIC":"13548.39","HRA":"6774.19","CONVEYANCE":"3387.10","OTHER":"3387.09"},
        "expected_employer_contributions":{"pf":"2600","esi":"0"},"expected_statement":{},
        "historical_lwp_included":false,"ready":true
    })).expect("fixture")
}

#[test]
fn advances_preserve_salary_and_deductions() {
    let first = calculate_period(&input()).expect("valid period");
    let mut no_advance = input();
    no_advance.advance_already_paid = Some("0".into());
    let second = calculate_period(&no_advance).expect("valid period");
    assert_eq!(first.gross, second.gross);
    assert_eq!(first.total_deductions, second.total_deductions);
    assert_eq!(first.net_earned, second.net_earned);
    assert_eq!(first.remaining_payable.to_string(), "19896.77");
    assert_eq!(first.lwp_amount.to_string(), "2903.23");
}

#[test]
fn paid_day_proration_does_not_charge_lwp_again() {
    let mut period = input();
    period.gross_rule = "PAID_DAYS_PLUS_OT".into();
    period.expected_earned_components.clear();
    assert_eq!(
        calculate_period(&period)
            .expect("valid period")
            .gross
            .to_string(),
        "27000.00"
    );
}

#[test]
fn unknown_employer_costs_remain_unknown_without_blocking_employee_salary() {
    let mut period = input();
    period
        .expected_employer_contributions
        .insert("esi".into(), None);
    let calculated = calculate_period(&period).expect("employee salary has complete source inputs");
    assert!(!calculated.employer.contains_key("esi"));
    assert_eq!(
        calculated.net_earned,
        calculate_period(&input()).unwrap().net_earned
    );
}

#[test]
fn additional_deduction_requires_a_reason() {
    let mut value = serde_json::to_value(input()).expect("fixture");
    value["additional_deductions"] = json!([{"code":"OTHER","amount":"100","reason":null}]);
    let period = serde_json::from_value(value).expect("fixture");
    assert!(calculate_period(&period).is_err());
}

#[test]
fn historical_usage_is_never_a_monthly_deduction() {
    let mut period = input();
    period.historical_lwp_included = true;
    assert!(calculate_period(&period).is_err());
}

#[test]
fn source_mismatch_prevents_finalization() {
    let mut period = input();
    period
        .expected_statement
        .insert("remaining_payable".into(), Some("1".into()));
    assert!(calculate_period(&period).is_err());
}

#[test]
fn employer_cost_does_not_change_gross_based_salary_split() {
    use kabipay_payroll::services::salary_rules::{validate_recurring_salary, RecurringSalary};
    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../hrms-database/import-templates/v1/example.synthetic.json"
    ))
    .unwrap();
    let salary: RecurringSalary =
        serde_json::from_value(example["employees"][0]["recurring_salary"].clone()).unwrap();
    let validated = validate_recurring_salary(&salary).unwrap();
    assert_eq!(validated.annual_gross.to_string(), "360000.00");
    assert_eq!(
        validated.annual_employer_pf.unwrap().to_string(),
        "35100.00"
    );
    assert_eq!(validated.annual_ctc.unwrap().to_string(), "395100.00");
    assert_eq!(validated.components["BASIC"].to_string(), "15000.00");
}
