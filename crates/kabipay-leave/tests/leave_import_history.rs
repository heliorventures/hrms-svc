use kabipay_leave::services::leave_import_history::{validate_opening, OpeningSnapshot};
use serde_json::json;
fn snapshot() -> OpeningSnapshot {
    serde_json::from_value(json!({"year":2026,"as_of":"2026-09-30","carry_forward":"4","grant":"6","source_taken":"12","source_balance":"-2",
        "paid_used":"10","paid_remaining":"0","pending":null,"planned":null,"ready":true})).unwrap()
}
#[test]
fn reconciled_excess_is_history_with_zero_paid_availability() {
    let result = validate_opening(&snapshot()).unwrap();
    assert_eq!(result.historical_lwp.to_string(), "2");
    assert_eq!(result.paid_remaining.to_string(), "0");
}
#[test]
fn negative_taken_and_unknown_grants_are_never_inverted_or_zeroed() {
    let mut value = snapshot();
    value.source_taken = Some("-12".into());
    assert!(validate_opening(&value).is_err());
    let mut value = snapshot();
    value.grant = None;
    assert!(validate_opening(&value).is_err());
}
#[test]
fn source_inconsistency_and_fabricated_paid_balance_are_rejected() {
    let mut value = snapshot();
    value.source_balance = Some("10".into());
    assert!(validate_opening(&value).is_err());
    let mut value = snapshot();
    value.paid_remaining = Some("10".into());
    assert!(validate_opening(&value).is_err());
}
