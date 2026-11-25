//! Actual domain writers; launched only by the owned loopback Node fixture.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::{context::ScopeType, important_email::*};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0007_employee_core::employee,
    d0015_expense::{expense, expense_category, expense_policy},
    d0025_workflow::{workflow, workflow_action, workflow_instance, workflow_step},
    d0027_communication_audit::notification,
    d0029_file_storage::file_storage,
    d0033_travel_request::travel_request,
};
use kabipay_expense::services::{
    approval_authority::ExpenseApprovalAuthority, expense_service as expenses,
    travel_request_service as travel,
};
use owned::*;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;
async fn events(owner: &DatabaseConnection, resource: Uuid) -> Vec<(Uuid, EmailEvent, String)> {
    owner.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,event,event_hash FROM important_email_intent WHERE resource_id=$1 ORDER BY created_at,id",[resource.into()])).await.unwrap().into_iter().map(|r|
 (r.try_get("","id").unwrap(),serde_json::from_value(r.try_get("","event").unwrap()).unwrap(),r.try_get("","event_hash").unwrap())).collect()
}
fn authority(user: u32) -> ExpenseApprovalAuthority {
    ExpenseApprovalAuthority {
        actor_user_id: id(user),
        actor_employee_id: Some(id(user + 100)),
        scope: ScopeType::All,
        permission: "expense:approve",
    }
}
async fn submit(source: &DatabaseConnection) -> expense::Model {
    expenses::submit_expense(
        source,
        id(1),
        id(2),
        id(5),
        id(20),
        Decimal::from(100),
        "INR",
        "2026-11-01".parse().unwrap(),
        "Synthetic claim",
        None,
        Some(id(21)),
    )
    .await
    .unwrap()
}
async fn submit_trip(source: &DatabaseConnection) -> travel_request::Model {
    travel::submit_travel_request(
        source,
        id(1),
        id(2),
        Some("Fixture A".into()),
        Some("Fixture B".into()),
        "2026-11-01".parse().unwrap(),
        "2026-11-02".parse().unwrap(),
        "Synthetic trip",
        None,
        "INR",
        id(5),
        Some(id(21)),
    )
    .await
    .unwrap()
}
#[tokio::test]
#[ignore = "owned Node fixture only; never use an application DSN"]
async fn owned_expense_and_travel_commit_rollback_next_step_outcome_and_replay() {
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
    entity(&owner, expense::Entity).await;
    entity(&owner, expense_category::Entity).await;
    entity(&owner, expense_policy::Entity).await;
    entity(&owner, workflow::Entity).await;
    entity(&owner, workflow_step::Entity).await;
    entity(&owner, workflow_instance::Entity).await;
    entity(&owner, workflow_action::Entity).await;
    entity(&owner, file_storage::Entity).await;
    entity(&owner, travel_request::Entity).await;
    entity(&owner, notification::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_expense").await;
    let request = submit(&source).await;
    let initial = events(&owner, request.id).await;
    assert_eq!(initial.len(), 2);
    assert!(initial
        .iter()
        .all(|(_, e, _)| e.kind == EmailEventKind::ActionRequired));
    assert!(initial
        .iter()
        .all(|(_, e, _)| e.recipient != EmailRecipientRef::LocalUser { user_id: id(5) }));
    expenses::approve_expense(&source, id(1), request.id, id(31), &authority(10), None)
        .await
        .unwrap();
    let advanced = events(&owner, request.id).await;
    assert_eq!(advanced.len(), 3);
    assert_eq!(advanced.iter().filter(|(_,e,_)|matches!(e.authorization_ref,EmailAuthorizationRef::LocalWorkflow{step_id,..}if step_id==id(32))).count(),1);
    expenses::approve_expense(&source, id(1), request.id, id(32), &authority(11), None)
        .await
        .unwrap();
    let all = events(&owner, request.id).await;
    assert_eq!(all.len(), 4);
    let (final_id, final_event, hash) = all
        .iter()
        .find(|(_, e, _)| e.kind == EmailEventKind::Outcome)
        .unwrap();
    assert_eq!(final_event.canonical_hash().unwrap(), *hash);
    let check = EmailEligibilityRequest {
        intent_id: *final_id,
        event_hash: hash.clone(),
        event: final_event.clone(),
    };
    assert_eq!(
        kabipay_expense::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &check
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    execute(
        &owner,
        &format!(
            "UPDATE expense SET payment_status='PAID',updated_at=clock_timestamp() WHERE id='{}'",
            request.id
        ),
    )
    .await;
    assert_eq!(
        kabipay_expense::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &check
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Eligible,
        "unrelated payment update cannot erase the recorded final outcome"
    );
    let stale = &initial[0];
    let stale = EmailEligibilityRequest {
        intent_id: stale.0,
        event_hash: stale.2.clone(),
        event: stale.1.clone(),
    };
    assert_eq!(
        kabipay_expense::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &stale
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    let before = count(&owner, "important_email_intent").await;
    assert!(
        expenses::approve_expense(&source, id(1), request.id, id(32), &authority(11), None)
            .await
            .is_err()
    );
    let tx = source.begin().await.unwrap();
    assert_eq!(
        append_email_intent(&tx, final_event).await.unwrap(),
        AppendIntentResult::ExistingSame
    );
    tx.commit().await.unwrap();
    assert_eq!(count(&owner, "important_email_intent").await, before);
    let rejected = submit(&source).await;
    expenses::reject_expense(
        &source,
        id(1),
        rejected.id,
        id(31),
        &authority(10),
        Some("Synthetic rejection".into()),
    )
    .await
    .unwrap();
    assert_eq!(
        events(&owner, rejected.id)
            .await
            .iter()
            .filter(|(_, e, _)| e.kind == EmailEventKind::Outcome)
            .count(),
        1
    );
    let mut trip_authority = authority(10);
    trip_authority.permission = "travel:approve";
    let trip = submit_trip(&source).await;
    assert_eq!(events(&owner, trip.id).await.len(), 2);
    travel::approve_travel_request(&source, id(1), trip.id, id(41), &trip_authority)
        .await
        .unwrap();
    trip_authority = authority(11);
    trip_authority.permission = "travel:approve";
    travel::approve_travel_request(&source, id(1), trip.id, id(42), &trip_authority)
        .await
        .unwrap();
    assert_eq!(events(&owner, trip.id).await.len(), 4);
    let rejected_trip = submit_trip(&source).await;
    travel::reject_travel_request(
        &source,
        id(1),
        rejected_trip.id,
        id(41),
        &trip_authority,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        events(&owner, rejected_trip.id)
            .await
            .iter()
            .filter(|(_, e, _)| e.kind == EmailEventKind::Outcome)
            .count(),
        1
    );
    assert!(
        count(&owner, "notification").await >= 4,
        "existing in-app outcomes remain intact"
    );
    let rollback_expense = submit(&source).await;
    let rollback_trip = submit_trip(&source).await;
    let (expenses_before, trips_before, instances_before, intents_before) = (
        count(&owner, "expense").await,
        count(&owner, "travel_request").await,
        count(&owner, "workflow_instance").await,
        count(&owner, "important_email_intent").await,
    );
    let decisions_before = transaction_counts(&owner).await;
    inject_intent_failure(&owner, true).await;
    assert!(expenses::approve_expense(
        &source,
        id(1),
        rollback_expense.id,
        id(31),
        &authority(10),
        None
    )
    .await
    .is_err());
    assert!(expenses::reject_expense(
        &source,
        id(1),
        rollback_expense.id,
        id(31),
        &authority(10),
        None
    )
    .await
    .is_err());
    trip_authority = authority(10);
    trip_authority.permission = "travel:approve";
    assert!(travel::approve_travel_request(
        &source,
        id(1),
        rollback_trip.id,
        id(41),
        &trip_authority
    )
    .await
    .is_err());
    assert!(travel::reject_travel_request(
        &source,
        id(1),
        rollback_trip.id,
        id(41),
        &trip_authority,
        None
    )
    .await
    .is_err());
    assert_eq!(
        transaction_counts(&owner).await,
        decisions_before,
        "failed next-step and outcome mail rolls back decisions"
    );
    assert!(expenses::submit_expense(
        &source,
        id(1),
        id(2),
        id(5),
        id(20),
        Decimal::from(100),
        "INR",
        "2026-11-01".parse().unwrap(),
        "Rollback fixture",
        None,
        Some(id(21))
    )
    .await
    .is_err());
    assert!(travel::submit_travel_request(
        &source,
        id(1),
        id(2),
        None,
        None,
        "2026-11-01".parse().unwrap(),
        "2026-11-02".parse().unwrap(),
        "Rollback fixture",
        None,
        "INR",
        id(5),
        Some(id(21))
    )
    .await
    .is_err());
    assert_eq!(
        (
            count(&owner, "expense").await,
            count(&owner, "travel_request").await,
            count(&owner, "workflow_instance").await,
            count(&owner, "important_email_intent").await
        ),
        (
            expenses_before,
            trips_before,
            instances_before,
            intents_before
        )
    );
    inject_intent_failure(&owner, false).await;
    expenses::approve_expense(
        &source,
        id(1),
        rollback_expense.id,
        id(31),
        &authority(10),
        None,
    )
    .await
    .unwrap();
    travel::approve_travel_request(&source, id(1), rollback_trip.id, id(41), &trip_authority)
        .await
        .unwrap();
    let before = transaction_counts(&owner).await;
    inject_intent_failure(&owner, true).await;
    assert!(expenses::approve_expense(
        &source,
        id(1),
        rollback_expense.id,
        id(32),
        &authority(11),
        None
    )
    .await
    .is_err());
    trip_authority = authority(11);
    trip_authority.permission = "travel:approve";
    assert!(travel::approve_travel_request(
        &source,
        id(1),
        rollback_trip.id,
        id(42),
        &trip_authority
    )
    .await
    .is_err());
    assert_eq!(
        transaction_counts(&owner).await,
        before,
        "final decision, encrypted business outbox and mail are atomic"
    );
    inject_intent_failure(&owner, false).await;
    assert!(source
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT credential_cipher FROM tenant_email_settings".to_owned()
        ))
        .await
        .is_err());
}
