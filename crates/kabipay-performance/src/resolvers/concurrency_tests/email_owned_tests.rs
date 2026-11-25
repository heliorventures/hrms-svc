//! Actual resolver and scheduler transactions in an admitted owned cluster.
use super::*;
use chrono::Datelike;
#[path = "../../../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use crate::services::{performance_administration, performance_workflow};
use kabipay_common::important_email::*;
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0007_employee_core::employee,
    d0018_performance::{kpi, review_cycle},
    d0075_performance_appraisal_lifecycle::{
        appraisal_answer, appraisal_question_option, continuous_feedback,
        performance_admin_exception,
    },
};
use owned::*;
async fn events(db: &DatabaseConnection) -> Vec<EmailEvent> {
    db.query_all(kabipay_organization_tenant::statement(
        "SELECT event FROM important_email_intent ORDER BY created_at,id",
        vec![],
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| serde_json::from_value(r.try_get("", "event").unwrap()).unwrap())
    .collect()
}
async fn eligible(db: &DatabaseConnection, event: &EmailEvent) -> EmailEligibilityDecision {
    crate::email_alerts::check_email_eligibility(
        db,
        &DatabaseConnection::Disconnected,
        &EmailEligibilityRequest {
            intent_id: Uuid::new_v4(),
            event: event.clone(),
            event_hash: event.canonical_hash().unwrap(),
        },
    )
    .await
    .unwrap()
}
async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(kabipay_organization_tenant::statement(sql, vec![]))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}
async fn run(
    db: &DatabaseConnection,
    actor: u32,
    employee: Option<u32>,
    operation: &str,
) -> async_graphql::Response {
    let (permission, scope) = match actor {
        5 => ("performance:self", "SELF"),
        11 => ("performance:evaluate", "TEAM"),
        _ => ("performance:manage", "ALL"),
    };
    let claims = ClientClaims {
        sub: id(actor),
        iss: "kabipay-client".into(),
        exp: 0,
        iat: 0,
        tenant_id: id(1),
        email: String::new(),
        employee_id: employee.map(id),
        must_change_password: false,
        roles: vec![],
        permissions: vec![permission.into()],
        permission_scopes: HashMap::from([(permission.into(), scope.into())]),
        resource_scopes: HashMap::new(),
    };
    Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        EmptySubscription,
    )
    .data(TestTenantDb(id(1), db.clone()))
    .data(DatabaseConnection::Disconnected)
    .data(TenantId(id(1)))
    .data(claims)
    .finish()
    .execute(operation)
    .await
}
async fn ok(
    db: &DatabaseConnection,
    actor: u32,
    employee: Option<u32>,
    op: &str,
) -> serde_json::Value {
    let r = run(db, actor, employee, op).await;
    assert!(r.errors.is_empty(), "{op}: {:?}", r.errors);
    r.data.into_json().unwrap()
}
#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_performance_transitions_replay_scheduler_reopen_and_rollback() {
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    execute(&owner, "CREATE SCHEMA tenant_fixture").await;
    entity(&owner, user::Entity).await;
    entity(&owner, user_role::Entity).await;
    entity(&owner, role::Entity).await;
    entity(&owner, role_permission::Entity).await;
    entity(&owner, permission::Entity).await;
    entity(&owner, permission_scope::Entity).await;
    entity(&owner, employee::Entity).await;
    entity(&owner, review_cycle::Entity).await;
    entity(&owner, goal::Entity).await;
    entity(&owner, kpi::Entity).await;
    entity(&owner, performance_program::Entity).await;
    entity(&owner, appraisal_template::Entity).await;
    entity(&owner, appraisal_template_section::Entity).await;
    entity(&owner, appraisal_question::Entity).await;
    entity(&owner, appraisal_question_option::Entity).await;
    entity(&owner, performance_participant::Entity).await;
    entity(&owner, appraisal_answer::Entity).await;
    entity(&owner, continuous_feedback::Entity).await;
    entity(&owner, performance_admin_exception::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_performance").await;
    let maintenance = fixture.connect("hrms_company_maintenance").await;
    let today = Utc::now().date_naive();
    let launch=format!("mutation {{ launchPerformanceCycle(input:{{performanceProgramId:\"{}\",appraisalTemplateId:\"{}\",periodDate:\"{today}\"}}){{id}} }}",id(20),id(21));
    let cycle = Uuid::parse_str(
        ok(&source, 10, None, &launch).await["launchPerformanceCycle"]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        events(&owner).await.is_empty(),
        "cycle launch without actionable goals is silent"
    );
    execute(&owner,&format!("UPDATE performance_participant SET is_excluded=true WHERE employee_id<>'{}';UPDATE performance_participant SET manager_employee_id='{}' WHERE employee_id='{}'",id(2),id(12),id(2))).await;
    let participant: Uuid = owner
        .query_one(kabipay_organization_tenant::statement(
            "SELECT id FROM performance_participant WHERE employee_id=$1 AND review_cycle_id=$2",
            vec![id(2).into(), cycle.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "id")
        .unwrap();
    let propose=format!("mutation {{ proposePerformanceGoal(input:{{participantId:\"{participant}\",title:\"PRIVATE GOAL\",weightage:\"100\"}}){{id status}} }}");
    inject_intent_failure(&owner, true).await;
    assert!(!run(&source, 5, Some(2), &propose).await.errors.is_empty());
    assert_eq!(
        scalar(&owner, "SELECT count(*)::bigint AS n FROM goal").await,
        0
    );
    inject_intent_failure(&owner, false).await;
    let goal = ok(&source, 5, Some(2), &propose).await["proposePerformanceGoal"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pending = events(&owner).await;
    assert_eq!(
        pending.len(),
        2,
        "account-authorized ALL admin without employment plus assigned TEAM manager"
    );
    for event in &pending {
        assert_eq!(event.kind, EmailEventKind::ActionRequired);
    }
    let admin_pending = pending
        .iter()
        .find(|e| e.recipient == EmailRecipientRef::LocalUser { user_id: id(10) })
        .unwrap();
    assert_eq!(
        eligible(&source, admin_pending).await,
        EmailEligibilityDecision::Eligible
    );
    execute(
        &owner,
        &format!("UPDATE role SET is_deleted=true WHERE id='{}'", id(210)),
    )
    .await;
    assert_eq!(
        eligible(&source, admin_pending).await,
        EmailEligibilityDecision::Forbidden,
        "queued action loses authority immediately with its custom role"
    );
    execute(
        &owner,
        &format!("UPDATE role SET is_deleted=false WHERE id='{}'", id(210)),
    )
    .await;
    let approve = format!(
        "mutation {{ approvePerformanceGoals(participantId:\"{participant}\"){{status}} }}"
    );
    inject_intent_failure(&owner, true).await;
    assert!(!run(&source, 11, Some(12), &approve).await.errors.is_empty());
    assert_eq!(
        scalar(
            &owner,
            "SELECT count(*)::bigint AS n FROM goal WHERE status='APPROVED'"
        )
        .await,
        0
    );
    inject_intent_failure(&owner, false).await;
    ok(&source, 11, Some(12), &approve).await;
    let approved = events(&owner).await;
    assert_eq!(approved.len(), 3);
    ok(&source, 11, Some(12), &approve).await;
    assert_eq!(
        events(&owner).await,
        approved,
        "unchanged approval is exactly once"
    );
    assert_eq!(
        eligible(&source, admin_pending).await,
        EmailEligibilityDecision::Obsolete
    );
    let advance = format!("mutation {{ advancePerformanceCycle(reviewCycleId:\"{cycle}\") }}");
    ok(&source, 10, None, &advance).await;
    assert_eq!(
        events(&owner).await,
        approved,
        "self-review stage is not an approval broadcast"
    );
    let submit=format!("mutation {{ submitSelfAppraisal(participantId:\"{participant}\",expectedRevision:1,answers:[{{questionId:\"{}\",textAnswer:\"PRIVATE SELF ANSWER\"}}]){{review{{id}}}} }}",id(23));
    ok(&source, 5, Some(2), &submit).await;
    assert_eq!(events(&owner).await, approved);
    // The automatic deadline path must emit the same actionable manager transition.
    execute(
        &owner,
        &format!("UPDATE review_cycle SET self_review_due_date=CURRENT_DATE WHERE id='{cycle}'"),
    )
    .await;
    inject_intent_failure(&owner, true).await;
    let sweep = performance_workflow::process_due_performance_stage_deadlines(
        &maintenance,
        &DatabaseConnection::Disconnected,
        id(1),
        today,
    )
    .await
    .unwrap();
    assert_eq!(sweep.stages_blocked, 1);
    assert_eq!(events(&owner).await, approved);
    assert_eq!(
        scalar(
            &owner,
            "SELECT count(*)::bigint AS n FROM review_cycle WHERE current_stage='SELF_REVIEW'"
        )
        .await,
        1
    );
    inject_intent_failure(&owner, false).await;
    let sweep = performance_workflow::process_due_performance_stage_deadlines(
        &maintenance,
        &DatabaseConnection::Disconnected,
        id(1),
        today,
    )
    .await
    .unwrap();
    assert_eq!(sweep.stages_advanced, 1);
    let manager_pending = events(&owner).await;
    assert_eq!(manager_pending.len(), 5);
    let manager=format!("mutation {{ submitManagerAppraisal(participantId:\"{participant}\",expectedRevision:1,finalRating:\"3\",answers:[{{questionId:\"{}\",textAnswer:\"PRIVATE MANAGER ANSWER\"}}]){{review{{id}}}} }}",id(23));
    inject_intent_failure(&owner, true).await;
    assert!(!run(&source, 11, Some(12), &manager).await.errors.is_empty());
    assert_eq!(scalar(&owner,"SELECT count(*)::bigint AS n FROM performance_participant WHERE manager_submitted_at IS NOT NULL").await,0);
    inject_intent_failure(&owner, false).await;
    ok(&source, 11, Some(12), &manager).await;
    assert_eq!(events(&owner).await.len(), 6);
    assert!(!run(&source, 11, Some(12), &manager).await.errors.is_empty());
    // Reopen is the actual transaction-owned administration function, not a fabricated adapter proof.
    inject_intent_failure(&owner, true).await;
    let tx = source.begin().await.unwrap();
    assert!(performance_administration::reopen_revision(
        &tx,
        &DatabaseConnection::Disconnected,
        id(1),
        participant,
        1,
        "MANAGER_REVIEW",
        "PRIVATE REASON",
        id(11)
    )
    .await
    .is_err());
    tx.rollback().await.unwrap();
    assert_eq!(events(&owner).await.len(), 6);
    inject_intent_failure(&owner, false).await;
    let tx = source.begin().await.unwrap();
    assert_eq!(
        performance_administration::reopen_revision(
            &tx,
            &DatabaseConnection::Disconnected,
            id(1),
            participant,
            1,
            "MANAGER_REVIEW",
            "PRIVATE REASON",
            id(11)
        )
        .await
        .unwrap(),
        2
    );
    tx.commit().await.unwrap();
    assert_eq!(
        events(&owner).await.len(),
        7,
        "acting manager excluded, current ALL admin notified"
    );
    let serialized = serde_json::to_string(&events(&owner).await).unwrap();
    for private in [
        "PRIVATE GOAL",
        "PRIVATE SELF ANSWER",
        "PRIVATE MANAGER ANSWER",
        "PRIVATE REASON",
        "example.invalid",
    ] {
        assert!(!serialized.contains(private));
    }
    // Manual stage activation uses the same emitter and exact recipient rules.
    execute(&owner,&format!("UPDATE performance_participant SET manager_submitted_at=NULL WHERE id='{participant}';UPDATE review_cycle SET current_stage='SELF_REVIEW' WHERE id='{cycle}'")).await;
    ok(&source, 10, None, &advance).await;
    assert_eq!(
        events(&owner).await.len(),
        8,
        "acting admin excluded, manager notified"
    );
    // Scheduled population has no configured goals: no generic cycle email.
    execute(&owner,&format!("UPDATE performance_program SET cadence='MONTHLY',anchor_date=CURRENT_DATE WHERE id='{}'",id(20))).await;
    let next = chrono::NaiveDate::from_ymd_opt(today.year() + 1, 1, 1).unwrap();
    let before = events(&owner).await;
    let created = performance_workflow::process_due_performance_cycles(
        &maintenance,
        &DatabaseConnection::Disconnected,
        id(1),
        next,
    )
    .await
    .unwrap();
    assert_eq!(created.cycles_created, 1);
    assert_eq!(events(&owner).await, before);
    // Goal edit/delete suppresses old actions through a changed canonical revision.
    let scheduled_participant:Uuid=owner.query_one(kabipay_organization_tenant::statement("SELECT p.id FROM performance_participant p JOIN review_cycle c ON c.id=p.review_cycle_id WHERE p.employee_id=$1 AND c.id<>$2",vec![id(2).into(),cycle.into()])).await.unwrap().unwrap().try_get("","id").unwrap();
    let propose = propose.replace(&participant.to_string(), &scheduled_participant.to_string());
    let next_goal = ok(&source, 5, Some(2), &propose).await["proposePerformanceGoal"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before_edit = events(&owner).await;
    let next_pending = before_edit.last().unwrap();
    assert_eq!(
        eligible(&source, next_pending).await,
        EmailEligibilityDecision::Eligible
    );
    let edit=format!("mutation {{ updatePerformanceGoal(goalId:\"{next_goal}\",input:{{participantId:\"{scheduled_participant}\",title:\"PRIVATE EDIT\",weightage:\"100\"}}){{id status}} }}");
    inject_intent_failure(&owner, true).await;
    assert!(!run(&source, 5, Some(2), &edit).await.errors.is_empty());
    assert_eq!(events(&owner).await, before_edit);
    inject_intent_failure(&owner, false).await;
    ok(&source, 5, Some(2), &edit).await;
    assert_eq!(
        eligible(&source, next_pending).await,
        EmailEligibilityDecision::Obsolete
    );
    let after_edit = events(&owner).await;
    let edited = after_edit.last().unwrap();
    assert_eq!(
        eligible(&source, edited).await,
        EmailEligibilityDecision::Eligible
    );
    let delete=format!("mutation {{ deletePerformanceGoal(participantId:\"{scheduled_participant}\",goalId:\"{next_goal}\") }}");
    ok(&source, 5, Some(2), &delete).await;
    assert_eq!(
        eligible(&source, edited).await,
        EmailEligibilityDecision::Obsolete
    );
    assert_eq!(
        events(&owner).await,
        after_edit,
        "deleting the only goal does not announce an incomplete goal set"
    );
    assert!(!goal.is_empty());
}
