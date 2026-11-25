//! Real local profile writers; all payloads/keys are synthetic fixture values.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::important_email::*;
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0007_employee_core::{employee, employee_aadhaar, employee_bank, employee_pan},
    d0008_document_system::employee_document,
    d0027_communication_audit::notification,
    d0050_employee_self_service::employee_profile_change_request,
};
use kabipay_employee::services::profile_change_service as profile;
use owned::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
fn input() -> profile::NewProfileChange {
    profile::NewProfileChange {
        request_type: profile::REQUEST_LEGAL.into(),
        first_name: Some("SyntheticPrivateValue".into()),
        last_name: None,
        date_of_birth: None,
        pan_number: None,
        aadhaar_number: None,
        bank_name: None,
        account_number: None,
        ifsc_code: None,
        account_type: None,
        supporting_document_id: None,
    }
}
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
#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_profile_commit_rollback_replay_cancel_and_private_payload() {
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
    entity(&owner, employee_bank::Entity).await;
    entity(&owner, employee_pan::Entity).await;
    entity(&owner, employee_aadhaar::Entity).await;
    entity(&owner, employee_document::Entity).await;
    entity(&owner, notification::Entity).await;
    entity(&owner, employee_profile_change_request::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_employee").await;
    inject_intent_failure(&owner, true).await;
    assert!(
        profile::submit_request(&source, id(1), id(2), id(5), input())
            .await
            .is_err()
    );
    assert!(events(&owner).await.is_empty());
    assert_eq!(count(&owner, "company_event_outbox").await, 0);
    assert_eq!(count(&owner, "notification").await, 0);
    inject_intent_failure(&owner, false).await;
    let request = profile::submit_request(&source, id(1), id(2), id(5), input())
        .await
        .unwrap();
    let initial = events(&owner).await;
    assert_eq!(initial.len(), 1);
    assert_eq!(
        initial[0].recipient,
        EmailRecipientRef::LocalUser { user_id: id(10) }
    );
    assert!(
        count(&owner, "notification").await >= 1,
        "existing in-app notification was preserved"
    );
    assert!(
        profile::submit_request(&source, id(1), id(2), id(5), input())
            .await
            .is_err()
    );
    assert_eq!(events(&owner).await, initial);
    inject_intent_failure(&owner, true).await;
    assert!(
        profile::resolve_request(&source, id(1), request.id, id(10), true, None)
            .await
            .is_err()
    );
    let row = owner
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT first_name FROM employee WHERE id=$1",
            [id(2).into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        row.try_get::<String>("", "first_name").unwrap(),
        "SyntheticPrivateValue"
    );
    assert_eq!(events(&owner).await, initial);
    assert!(profile::resolve_request(
        &source,
        id(1),
        request.id,
        id(10),
        false,
        Some("synthetic review".into())
    )
    .await
    .is_err());
    inject_intent_failure(&owner, false).await;
    assert_eq!(
        profile::resolve_request(&source, id(1), request.id, id(10), true, None)
            .await
            .unwrap()
            .status,
        "APPROVED"
    );
    assert_eq!(events(&owner).await.len(), 2);
    let second = profile::submit_request(&source, id(1), id(2), id(5), input())
        .await
        .unwrap();
    let before_cancel = events(&owner).await;
    profile::cancel_request(&source, id(1), second.id, id(5), false)
        .await
        .unwrap();
    assert_eq!(events(&owner).await, before_cancel);
    let obsolete = before_cancel.last().unwrap().clone();
    let check = EmailEligibilityRequest {
        intent_id: id(90),
        event_hash: obsolete.canonical_hash().unwrap(),
        event: obsolete,
    };
    assert_eq!(
        kabipay_employee::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &check
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    let third = profile::submit_request(&source, id(1), id(2), id(5), input())
        .await
        .unwrap();
    profile::resolve_request(
        &source,
        id(1),
        third.id,
        id(10),
        false,
        Some("synthetic review".into()),
    )
    .await
    .unwrap();
    assert_eq!(events(&owner).await.len(), 5);
    assert!(events(&owner).await.iter().all(|event| {
        let json = serde_json::to_string(event).unwrap();
        !json.contains("SyntheticPrivateValue") && !json.contains("synthetic review")
    }));
}
