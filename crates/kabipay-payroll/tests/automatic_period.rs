use kabipay_payroll::services::automatic_period::{new_input, refresh_lwp};
use serde_json::json;

#[test]
fn new_month_uses_employee_configuration_without_inventing_eligibility() {
    let input = new_input(2026, 10).unwrap();
    let automatic = input.automatic.unwrap();
    assert!(automatic.use_employee_configuration);
    assert_eq!(automatic.eligibility.pf_applicable, None);
    assert_eq!(automatic.eligibility.esi_applicable, None);
    assert_eq!(input.incentive.as_deref(), Some("0"));
    assert_eq!(input.advance_already_paid.as_deref(), Some("0"));
    assert_eq!(input.month_days.as_deref(), Some("31"));
    assert!(input.expected_earned_components.is_empty());
}

#[test]
fn refresh_approved_leave_preserves_monthly_exceptions() {
    let mut input = new_input(2026, 10).unwrap();
    input.incentive = Some("1750".into());
    input.advance_already_paid = Some("500".into());
    refresh_lwp(&mut input, &json!({"days":"2.5","hash":"approved-v2"})).unwrap();
    assert_eq!(input.lwp_days.as_deref(), Some("2.5"));
    assert_eq!(
        input.approved_lwp_review_hash.as_deref(),
        Some("approved-v2")
    );
    assert_eq!(input.incentive.as_deref(), Some("1750"));
    assert_eq!(input.advance_already_paid.as_deref(), Some("500"));
}

#[test]
fn explicit_unpaid_leave_exception_requires_reason_and_cannot_hide_approved_days() {
    let mut input = new_input(2026, 10).unwrap();
    let mut value = serde_json::to_value(&input).unwrap();
    value["automatic"]["lwp_override"] = json!({"days":"1","reason":"Confirmed correction"});
    input = serde_json::from_value(value).unwrap();
    assert!(refresh_lwp(&mut input, &json!({"days":"2","hash":"v1"})).is_err());
    let mut value = serde_json::to_value(&input).unwrap();
    value["automatic"]["lwp_override"] = json!({"days":"3","reason":""});
    input = serde_json::from_value(value).unwrap();
    assert!(refresh_lwp(&mut input, &json!({"days":"2","hash":"v1"})).is_err());
}

#[test]
fn invalid_calendar_period_is_rejected() {
    assert!(new_input(2026, 13).is_err());
}
