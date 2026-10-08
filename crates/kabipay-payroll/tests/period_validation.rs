use kabipay_payroll::services::payroll_rules::{
    calculate_period, AdditionalDeduction, PeriodInput,
};
fn input() -> PeriodInput {
    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../hrms-database/import-templates/v1/example.synthetic.json"
    ))
    .unwrap();
    serde_json::from_value(example["employees"][0]["period_input"].clone()).unwrap()
}
#[test]
fn advances_and_lwp_cannot_be_disguised_as_additional_deductions() {
    for code in ["ADVANCE", "LWP", "PF"] {
        let mut period = input();
        period.additional_deductions.push(AdditionalDeduction {
            code: code.into(),
            amount: Some("1".into()),
            reason: Some("An explanation cannot authorize duplicate attribution".into()),
            origin: None,
        });
        period.expected_statement.clear();
        assert!(calculate_period(&period).is_err());
    }
}
#[test]
fn unpaid_leave_must_use_the_configured_gross_basis_and_bounded_days() {
    for (basis, days) in [("BASIC", "1"), ("GROSS", "400")] {
        let mut period = input();
        period.lwp_basis = Some(basis.into());
        period.lwp_days = Some(days.into());
        period.expected_statement.clear();
        assert!(calculate_period(&period).is_err());
    }
}
