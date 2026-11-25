//! Actual command transactions under Payroll's restricted login.
#[path = "support/fixture.rs"]
mod fixture;
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::important_email::*;
use kabipay_db_entities::tenant::d0005_auth_rbac::{
    permission, permission_scope, role, role_permission, user_role,
};
use kabipay_loans::*;
use kabipay_loans_domain::*;
use owned::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
async fn events(owner: &DatabaseConnection) -> Vec<EmailEvent> {
    owner
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT event FROM important_email_intent ORDER BY created_at,id".to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| serde_json::from_value(row.try_get("", "event").unwrap()).unwrap())
        .collect()
}
async fn counts(owner: &DatabaseConnection) -> (i64, i64, i64, i64) {
    let row=owner.query_one(Statement::from_string(DbBackend::Postgres,"SELECT (SELECT count(*) FROM loan_request) AS r,(SELECT count(*) FROM workflow_action) AS a,(SELECT count(*) FROM loan_command_receipt) AS c,(SELECT count(*) FROM loan_account) AS l".to_owned())).await.unwrap().unwrap();
    (
        row.try_get("", "r").unwrap(),
        row.try_get("", "a").unwrap(),
        row.try_get("", "c").unwrap(),
        row.try_get("", "l").unwrap(),
    )
}
fn intermediate(
    f: &fixture::Fixture,
    request: &LoanCommandResult,
    decision: RequestDecision,
) -> LoanCommand {
    LoanCommand::DecideRequest {
        request_id: request.record_id,
        step_id: f.step,
        decision,
        reason: "Synthetic review reason".into(),
        terms: None,
        effective_from: None,
        first_due_date: None,
        agreement_reference: None,
    }
}
#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_loan_command_stage_outcome_dedup_and_atomic_failure() {
    let owned = OwnedEmailFixture::load();
    let owner = owned.connect("fixture_owner").await;
    entity(&owner, user_role::Entity).await;
    entity(&owner, role::Entity).await;
    entity(&owner, role_permission::Entity).await;
    entity(&owner, permission::Entity).await;
    entity(&owner, permission_scope::Entity).await;
    synthetic_defaults(&owner).await;
    owned.install_owned_sql(&owner).await;
    let entitlement = kabipay_common::entitlements::Entitlements::load(&owner, id(1))
        .await
        .unwrap();
    let mut f = fixture::Fixture {
        db: owned.connect("hrms_company_payroll").await,
        entitlements: Some(entitlement),
        tenant: id(1),
        employee: id(2),
        user: id(5),
        other: id(3),
        other_user: id(6),
        manager: id(10),
        workflow: id(30),
        step: id(31),
        policy: uuid::Uuid::nil(),
        today: chrono::Utc::now().date_naive(),
    };
    f.policy = f
        .apply(
            f.manager,
            "ALL",
            1,
            "policy",
            LoanCommand::PublishPolicy {
                key: "fixture".into(),
                currency: f.currency(),
                effective_from: f.today,
                effective_to: None,
                rules: fixture::fixture_policy_rules(f.workflow, 100000),
            },
        )
        .await
        .unwrap()
        .record_id;
    let command = LoanCommand::SubmitRequest {
        request_id: None,
        employee_id: None,
        policy_id: f.policy,
        amount: rust_decimal::Decimal::from(10000),
        purpose: "Synthetic purpose".into(),
        notes: None,
        preferences: serde_json::json!({}),
    };
    inject_intent_failure(&owner, true).await;
    let before = counts(&owner).await;
    assert!(f
        .apply(f.user, "SELF", 1, "request", command.clone())
        .await
        .is_err());
    assert_eq!(counts(&owner).await, before);
    assert!(events(&owner).await.is_empty());
    inject_intent_failure(&owner, false).await;
    let request = f
        .apply(f.user, "SELF", 1, "request", command.clone())
        .await
        .unwrap();
    assert_eq!(events(&owner).await.len(), 2);
    let replay = f
        .apply(f.user, "SELF", 1, "request", command)
        .await
        .unwrap();
    assert_eq!(replay.record_id, request.record_id);
    assert_eq!(events(&owner).await.len(), 2);
    inject_intent_failure(&owner, true).await;
    let before = counts(&owner).await;
    assert!(f
        .apply(
            f.manager,
            "ALL",
            request.version,
            "intermediate",
            intermediate(&f, &request, RequestDecision::Approve)
        )
        .await
        .is_err());
    assert_eq!(counts(&owner).await, before);
    inject_intent_failure(&owner, false).await;
    let progress = f
        .apply(
            f.manager,
            "ALL",
            request.version,
            "intermediate",
            intermediate(&f, &request, RequestDecision::Approve),
        )
        .await
        .unwrap();
    assert_eq!(progress.state, "UNDER_REVIEW");
    assert_eq!(events(&owner).await.len(), 3);
    let mut final_command = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest { step_id, .. } = &mut final_command {
        *step_id = id(32);
    }
    inject_intent_failure(&owner, true).await;
    let before = counts(&owner).await;
    assert!(f
        .apply(
            id(11),
            "ALL",
            progress.version,
            "final",
            final_command.clone()
        )
        .await
        .is_err());
    assert_eq!(counts(&owner).await, before);
    inject_intent_failure(&owner, false).await;
    let final_result = f
        .apply(
            id(11),
            "ALL",
            progress.version,
            "final",
            final_command.clone(),
        )
        .await
        .unwrap();
    assert_eq!(final_result.state, "APPROVED");
    assert_eq!(events(&owner).await.len(), 4);
    assert_eq!(
        f.apply(id(11), "ALL", progress.version, "final", final_command)
            .await
            .unwrap()
            .record_id,
        request.record_id
    );
    assert_eq!(events(&owner).await.len(), 4);
    let rejected = f.request(f.user, "reject-request").await;
    inject_intent_failure(&owner, true).await;
    let before = counts(&owner).await;
    assert!(f
        .apply(
            f.manager,
            "ALL",
            rejected.version,
            "reject",
            intermediate(&f, &rejected, RequestDecision::Reject)
        )
        .await
        .is_err());
    assert_eq!(counts(&owner).await, before);
    inject_intent_failure(&owner, false).await;
    assert_eq!(
        f.apply(
            f.manager,
            "ALL",
            rejected.version,
            "reject",
            intermediate(&f, &rejected, RequestDecision::Reject)
        )
        .await
        .unwrap()
        .state,
        "REJECTED"
    );
    let withdrawn = f.request(f.user, "withdraw-request").await;
    let before = events(&owner).await.len();
    f.apply(
        f.user,
        "SELF",
        withdrawn.version,
        "withdraw",
        LoanCommand::WithdrawRequest {
            request_id: withdrawn.record_id,
            reason: "Synthetic withdrawal".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(events(&owner).await.len(), before);
    assert!(events(&owner)
        .await
        .iter()
        .all(|event| !serde_json::to_string(event).unwrap().contains("Synthetic")));
    execute(
        &owner,
        "UPDATE workflow_step SET approver_permission='expense:approve' WHERE sequence_order=2",
    )
    .await;
    assert!(matches!(
        configured_loan_approval_step(&f.db, f.tenant, f.workflow).await,
        Err(LoanModuleError::PolicyUnavailable)
    ));
}
