fn writer(source: &'static str, name: &str) -> &'static str {
    let start = source.find(&format!("pub async fn {name}(")).unwrap();
    let rest = &source[start..];
    let end = rest[1..]
        .find("\npub async fn ")
        .map_or(rest.len(), |i| i + 1);
    &rest[..end]
}
#[test]
fn expense_and_travel_submit_emit_only_the_active_workflow_before_commit() {
    for (source, name) in [
        (
            include_str!("../src/services/expense_service.rs"),
            "submit_expense",
        ),
        (
            include_str!("../src/services/travel_request_service.rs"),
            "submit_travel_request",
        ),
    ] {
        let body = writer(source, name);
        assert!(
            body.find("emit_active_step")
                .is_some_and(|i| i < body.find("txn.commit()").unwrap()),
            "{name}"
        );
    }
}
#[test]
fn approval_and_rejection_use_transactional_bridges_and_keep_in_app_after_commit() {
    for (source, approve, reject) in [
        (
            include_str!("../src/services/expense_service.rs"),
            "approve_expense",
            "reject_expense",
        ),
        (
            include_str!("../src/services/travel_request_service.rs"),
            "approve_travel_request",
            "reject_travel_request",
        ),
    ] {
        let body = writer(source, approve);
        assert!(body.contains("emit_active_step"), "{approve}");
        assert!(body.contains("emit_outcome"));
        assert!(body.contains("after_successful_commit"));
        let body = writer(source, reject);
        assert!(
            body.find("emit_outcome")
                .is_some_and(|i| i < body.find("txn.commit()").unwrap()),
            "{reject}"
        );
        assert!(body.contains("after_successful_commit"));
    }
}
