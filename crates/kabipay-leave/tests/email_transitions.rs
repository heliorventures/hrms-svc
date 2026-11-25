// Source bridge alarms complement the real transaction rehearsals; they are
// not assertions that a live business request has been submitted/approved.
fn writer(name: &str) -> &'static str {
    let source = include_str!("../src/services/leave_service.rs");
    let start = source.find(&format!("pub async fn {name}(")).unwrap();
    let rest = &source[start..];
    let end = rest[1..]
        .find("\npub async fn ")
        .map_or(rest.len(), |i| i + 1);
    &rest[..end]
}
#[test]
fn submit_leave_appends_active_step_before_commit() {
    let source = writer("submit_leave_request");
    assert!(source
        .find("emit_active_step")
        .is_some_and(|i| i < source.find("txn.commit()").unwrap()));
}
#[test]
fn local_approval_emits_next_step_and_final_outcome_inside_their_transactions() {
    let source = writer("approve_leave_request");
    assert!(source
        .find("emit_active_step")
        .is_some_and(|i| i < source.find("txn.commit()").unwrap()));
    assert!(source.contains("emit_outcome"));
}
#[test]
fn organization_decision_uses_the_same_transaction_owned_email_helpers() {
    let source = writer("decide_organization_leave");
    assert!(source.contains("emit_active_step"));
    assert!(source.contains("emit_outcome"));
    assert!(!source.contains(".commit()"));
}
#[test]
fn rejection_withdrawal_and_approved_comp_off_cancellation_have_explicit_outcome_bridges() {
    for name in [
        "reject_leave_request",
        "cancel_leave_request",
        "cancel_approved_comp_off_leave",
    ] {
        let source = writer(name);
        assert!(
            source
                .find("emit_outcome")
                .is_some_and(|i| i < source.find("txn.commit()").unwrap()),
            "{name}"
        );
    }
}
