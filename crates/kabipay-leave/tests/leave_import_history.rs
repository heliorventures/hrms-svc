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

#[test]
fn normalized_snapshot_preserves_raw_evidence_without_using_it_as_entitlement() {
    let value: OpeningSnapshot = serde_json::from_value(json!({
        "year":2026,"as_of":"2026-08-31","carry_forward":"0","grant":"0",
        "source_taken":"6","source_balance":"-6","paid_used":"0","paid_remaining":"0",
        "pending":null,"planned":null,"ready":true,
        "raw_source_values":{"carry_forward":null,"grant":"0","taken":"6","balance":"0"}
    }))
    .unwrap();
    assert_eq!(
        validate_opening(&value).unwrap().historical_lwp.to_string(),
        "6"
    );
    assert_eq!(
        serde_json::to_value(value).unwrap()["raw_source_values"]["balance"],
        "0"
    );
}
