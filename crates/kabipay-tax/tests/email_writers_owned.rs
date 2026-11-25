//! Only the owned-loopback Node fixture may launch this integration test.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::{context::ScopeType, important_email::*};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0007_employee_core::employee,
    d0013_tax_statutory::{tax_configuration_version, tax_section_definition},
    d0027_communication_audit::notification,
    d0029_file_storage::file_storage,
    d0031_tax_proof::tax_proof_line,
    d0093_tax_projection_configuration::{employee_tax_declaration, employee_tax_settings},
};
use kabipay_tax::services::tax_service as tax;
use owned::*;
use rust_decimal::Decimal;
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
async fn submit(source: &DatabaseConnection, section: &str) -> tax_proof_line::Model {
    tax::submit_tax_proof_line(
        source,
        id(1),
        id(2),
        id(5),
        Some(id(20)),
        2026,
        section.into(),
        Decimal::from(100),
        Decimal::from(100),
        id(21),
        Some("OLD".into()),
    )
    .await
    .unwrap()
}
#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_tax_submission_decisions_rollback_dedup_and_current_eligibility() {
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
    entity(&owner, tax_section_definition::Entity).await;
    entity(&owner, tax_configuration_version::Entity).await;
    entity(&owner, notification::Entity).await;
    entity(&owner, file_storage::Entity).await;
    entity(&owner, tax_proof_line::Entity).await;
    entity(&owner, employee_tax_settings::Entity).await;
    entity(&owner, employee_tax_declaration::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_tax").await;
    // Actual writes use a custom permission-named role, not Admin/HR labels.
    let first = submit(&source, "80C").await;
    let initial = events(&owner).await;
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].kind, EmailEventKind::ActionRequired);
    assert_eq!(
        initial[0].recipient,
        EmailRecipientRef::LocalUser { user_id: id(10) }
    );
    assert_eq!(submit(&source, "80C").await.id, first.id);
    assert_eq!(
        events(&owner).await,
        initial,
        "identical pending submission is a replay, not another approval request"
    );
    inject_intent_failure(&owner, true).await;
    let before = events(&owner).await;
    assert!(
        tax::approve_tax_proof_line(&source, id(1), first.id, ScopeType::All, id(110), id(10))
            .await
            .is_err()
    );
    assert_eq!(events(&owner).await, before);
    let row = owner
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT status FROM tax_proof_line WHERE id=$1",
            [first.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "PENDING");
    assert!(tax::reject_tax_proof_line(
        &source,
        id(1),
        first.id,
        ScopeType::All,
        id(110),
        Some("synthetic review".into())
    )
    .await
    .is_err());
    let failed = tax::submit_tax_proof_line(
        &source,
        id(1),
        id(2),
        id(5),
        Some(id(20)),
        2026,
        "80D".into(),
        Decimal::from(100),
        Decimal::from(100),
        id(21),
        Some("OLD".into()),
    )
    .await;
    assert!(failed.is_err());
    inject_intent_failure(&owner, false).await;
    assert_eq!(
        tax::approve_tax_proof_line(&source, id(1), first.id, ScopeType::All, id(110), id(10))
            .await
            .unwrap()
            .status,
        "APPROVED"
    );
    let all = events(&owner).await;
    assert_eq!(all.len(), 2);
    let outcome = all
        .iter()
        .find(|e| e.kind == EmailEventKind::Outcome)
        .unwrap()
        .clone();
    let request = EmailEligibilityRequest {
        intent_id: id(90),
        event_hash: outcome.canonical_hash().unwrap(),
        event: outcome,
    };
    assert_eq!(
        kabipay_tax::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &request
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    let pending = submit(&source, "80C").await;
    assert_eq!(
        kabipay_tax::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &request
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    assert_eq!(
        tax::reject_tax_proof_line(
            &source,
            id(1),
            pending.id,
            ScopeType::All,
            id(110),
            Some("synthetic review".into())
        )
        .await
        .unwrap()
        .status,
        "REJECTED"
    );
    let completed = events(&owner).await;
    assert_eq!(completed.len(), 4);
    assert!(completed.iter().all(|e| !serde_json::to_string(e)
        .unwrap()
        .contains("synthetic review")));
}
