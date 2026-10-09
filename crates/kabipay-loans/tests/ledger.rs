use kabipay_loans::{command_hash, CommandMeta};
use serde_json::json;
#[test]
fn command_identity_is_stable_for_reordered_json_and_changes_with_payload() {
    let a = command_hash(&json!({"amount":"1000","loan":"abc"})).unwrap();
    let b = command_hash(&json!({"loan":"abc","amount":"1000"})).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.len(), 64);
    assert_ne!(
        a,
        command_hash(&json!({"loan":"abc","amount":"1001"})).unwrap()
    );
}
#[test]
fn mutation_requires_nonempty_bounded_key_and_positive_expected_version() {
    assert!(CommandMeta {
        idempotency_key: "receipt-203".into(),
        expected_version: 1
    }
    .validate()
    .is_ok());
    for m in [
        CommandMeta {
            idempotency_key: " ".into(),
            expected_version: 1,
        },
        CommandMeta {
            idempotency_key: "a".repeat(129),
            expected_version: 1,
        },
        CommandMeta {
            idempotency_key: "key".into(),
            expected_version: 0,
        },
    ] {
        assert!(m.validate().is_err());
    }
}
