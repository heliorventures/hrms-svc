// Caller alarms accompany actual owned commit/rollback/authorization rehearsals.
#[test]
fn manual_and_automatic_advances_share_one_cycle_email_hook() {
    let source = include_str!("../src/services/performance_administration.rs");
    let core = source.find("async fn advance_cycle_with_due_date").unwrap();
    assert!(source[core..].contains("emit_performance_cycle"));
    assert!(source.contains("advance_cycle_with_due_date"));
    assert!(
        include_str!("../src/services/performance_workflow.rs").contains("emit_performance_cycle")
    );
}
#[test]
fn actual_goal_and_appraisal_transitions_use_owned_participant_hook() {
    let source = include_str!("../src/resolvers/mutation.rs");
    for name in [
        "propose_performance_goal",
        "update_performance_goal",
        "delete_performance_goal",
        "approve_performance_goals",
        "submit_self_appraisal",
        "submit_manager_appraisal",
        "acknowledge_performance_review",
    ] {
        let start = source.find(&format!("async fn {name}(")).unwrap();
        let body = &source[start..];
        let end = body[1..]
            .find("\n    async fn ")
            .map_or(body.len(), |i| i + 1);
        assert!(
            body[..end].contains("emit_performance_participant"),
            "{name}"
        );
    }
}
#[test]
fn organization_review_uses_same_hook_with_verified_actor_aliases() {
    let source = include_str!("../src/organization.rs");
    assert!(source.contains("emit_performance_participant"));
    assert!(source.contains("actor_target_user_ids"));
}

#[test]
fn reopened_review_revision_emits_only_its_new_actionable_assignment() {
 let source=include_str!("../src/services/performance_administration.rs");
 let start=source.find("pub async fn reopen_revision(").unwrap();
 let body=&source[start..source.find("pub async fn create_or_keep_exception").unwrap()];
 assert!(body.contains("emit_performance_participant"));
}
