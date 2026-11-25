//! Joined signed foreign source-command acceptance against an owned policy responder.
//! Actual source mutation, mail proofs, recipient SQL, store and SMTP fence run; no provider socket.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::important_email::*;
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
async fn owned_signed_foreign_decision_to_fenced_smtp() {
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
    joined::run(&owner, &source).await;
}
#[path = "support/email_signed_foreign.rs"]
mod joined;
