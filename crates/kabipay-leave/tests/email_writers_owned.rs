//! Real local Leave writers; only the owned Node fixture launches this test.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::{context::ScopeType, important_email::*};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0007_employee_core::employee,
    d0010_time_shift_roster::{holiday, holiday_calendar},
    d0011_leave::{leave_balance, leave_request, leave_type},
    d0025_workflow::{workflow, workflow_action, workflow_instance, workflow_step},
    d0027_communication_audit::notification,
    d0078_comp_off::{comp_off_allocation, comp_off_credit, comp_off_policy},
    d0097_location_working_calendar::working_calendar_profile,
    d0098_leave_working_dates::leave_working_date_snapshot,
};
use kabipay_leave::services::leave_service as leave;
use owned::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;
async fn events(owner: &DatabaseConnection, resource: Uuid) -> Vec<(Uuid, EmailEvent, String)> {
    owner.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,event,event_hash FROM important_email_intent WHERE resource_id=$1 ORDER BY created_at,id",[resource.into()])).await.unwrap().into_iter().map(|r|
 (r.try_get("","id").unwrap(),serde_json::from_value(r.try_get("","event").unwrap()).unwrap(),r.try_get("","event_hash").unwrap())).collect()
}
async fn submit(source: &DatabaseConnection, date: &str) -> leave_request::Model {
    let date = date.parse().unwrap();
    leave::submit_leave_request(
        source,
        &DatabaseConnection::Disconnected,
        id(1),
        "2026-11-01".parse().unwrap(),
        id(5),
        Some(id(2)),
        id(20),
        date,
        date,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap()
}
#[tokio::test]
#[ignore = "owned Node fixture only; no live DSN"]
async fn owned_leave_commit_rollback_next_step_cancellation_and_final_outcome() {
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
    entity(&owner, leave_request::Entity).await;
    entity(&owner, leave_type::Entity).await;
    entity(&owner, leave_balance::Entity).await;
    entity(&owner, workflow::Entity).await;
    entity(&owner, workflow_step::Entity).await;
    entity(&owner, workflow_instance::Entity).await;
    entity(&owner, workflow_action::Entity).await;
    entity(&owner, notification::Entity).await;
    entity(&owner, holiday::Entity).await;
    entity(&owner, holiday_calendar::Entity).await;
    entity(&owner, working_calendar_profile::Entity).await;
    entity(&owner, leave_working_date_snapshot::Entity).await;
    entity(&owner, comp_off_policy::Entity).await;
    entity(&owner, comp_off_credit::Entity).await;
    entity(&owner, comp_off_allocation::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_leave").await;
    let metadata = DatabaseConnection::Disconnected;
    let today = "2026-11-01".parse().unwrap();
    let request = submit(&source, "2026-11-02").await;
    let initial = events(&owner, request.id).await;
    assert_eq!(initial.len(), 2);
    assert!(initial
        .iter()
        .all(|(_, e, _)| e.category == EmailCategory::Leave
            && e.kind == EmailEventKind::ActionRequired));
    leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        request.id,
        id(31),
        id(10),
        Some(id(110)),
        ScopeType::All,
    )
    .await
    .unwrap();
    let advanced = events(&owner, request.id).await;
    assert_eq!(advanced.len(), 3);
    assert_eq!(advanced.iter().filter(|(_,e,_)|matches!(e.authorization_ref,EmailAuthorizationRef::LocalWorkflow{step_id,..}if step_id==id(32))).count(),1);
    leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        request.id,
        id(32),
        id(11),
        Some(id(111)),
        ScopeType::All,
    )
    .await
    .unwrap();
    let final_events = events(&owner, request.id).await;
    assert_eq!(final_events.len(), 4);
    let outcome = final_events
        .iter()
        .find(|(_, e, _)| e.kind == EmailEventKind::Outcome)
        .unwrap();
    let check = EmailEligibilityRequest {
        intent_id: outcome.0,
        event_hash: outcome.2.clone(),
        event: outcome.1.clone(),
    };
    assert_eq!(
        kabipay_leave::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    let stale = &initial[0];
    let check = EmailEligibilityRequest {
        intent_id: stale.0,
        event_hash: stale.2.clone(),
        event: stale.1.clone(),
    };
    assert_eq!(
        kabipay_leave::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    let before = count(&owner, "important_email_intent").await;
    assert!(leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        request.id,
        id(32),
        id(11),
        Some(id(111)),
        ScopeType::All
    )
    .await
    .is_err());
    assert_eq!(count(&owner, "important_email_intent").await, before);
    let rejected = submit(&source, "2026-11-03").await;
    leave::reject_leave_request(
        &source,
        id(1),
        today,
        rejected.id,
        id(31),
        id(10),
        Some(id(110)),
        ScopeType::All,
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
    let withdrawn = submit(&source, "2026-11-04").await;
    leave::cancel_leave_request(&source, id(1), today, withdrawn.id, id(5), Some(id(2)))
        .await
        .unwrap();
    assert_eq!(
        events(&owner, withdrawn.id).await.len(),
        2,
        "withdrawal does not email its own actor"
    );
    let stale = events(&owner, withdrawn.id).await.remove(0);
    let check = EmailEligibilityRequest {
        intent_id: stale.0,
        event_hash: stale.2,
        event: stale.1,
    };
    assert_eq!(
        kabipay_leave::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    let rollback_request = submit(&source, "2026-11-05").await;
    let (requests, instances, intents) = (
        count(&owner, "leave_request").await,
        count(&owner, "workflow_instance").await,
        count(&owner, "important_email_intent").await,
    );
    let decisions_before = transaction_counts(&owner).await;
    inject_intent_failure(&owner, true).await;
    assert!(leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        rollback_request.id,
        id(31),
        id(10),
        Some(id(110)),
        ScopeType::All
    )
    .await
    .is_err());
    assert!(leave::reject_leave_request(
        &source,
        id(1),
        today,
        rollback_request.id,
        id(31),
        id(10),
        Some(id(110)),
        ScopeType::All,
        None
    )
    .await
    .is_err());
    assert!(
        leave::cancel_approved_comp_off_leave(&source, id(1), today, id(60), id(10))
            .await
            .is_err()
    );
    assert_eq!(
        transaction_counts(&owner).await,
        decisions_before,
        "workflow, audit and mail roll back together"
    );
    let date = "2026-11-06".parse().unwrap();
    assert!(leave::submit_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        id(5),
        Some(id(2)),
        id(20),
        date,
        date,
        false,
        None,
        None,
        None,
        None
    )
    .await
    .is_err());
    assert_eq!(
        (
            count(&owner, "leave_request").await,
            count(&owner, "workflow_instance").await,
            count(&owner, "important_email_intent").await
        ),
        (requests, instances, intents)
    );
    inject_intent_failure(&owner, false).await;
    leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        rollback_request.id,
        id(31),
        id(10),
        Some(id(110)),
        ScopeType::All,
    )
    .await
    .unwrap();
    let before = transaction_counts(&owner).await;
    inject_intent_failure(&owner, true).await;
    assert!(leave::approve_leave_request(
        &source,
        &metadata,
        id(1),
        today,
        rollback_request.id,
        id(32),
        id(11),
        Some(id(111)),
        ScopeType::All
    )
    .await
    .is_err());
    assert_eq!(
        transaction_counts(&owner).await,
        before,
        "final approval and encrypted webhook cannot outlive failed mail insertion"
    );
    inject_intent_failure(&owner, false).await;
    leave::cancel_approved_comp_off_leave(&source, id(1), today, id(60), id(10))
        .await
        .unwrap();
    assert_eq!(
        events(&owner, id(60))
            .await
            .iter()
            .filter(|(_, e, _)| e.kind == EmailEventKind::Outcome)
            .count(),
        1
    );
    assert!(
        count(&owner, "notification").await >= 4,
        "in-app outcomes are unchanged"
    );
}
