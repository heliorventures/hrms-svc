//! Existing private service entrypoints, using real owned PostgreSQL transactions.
use super::*;
#[path = "../../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use kabipay_common::{context::ScopeType, important_email::*};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{permission, permission_scope, role, role_permission, user, user_role},
    d0027_communication_audit::notification,
    d0028_master_data::master_data,
};
use owned::*;
async fn events(owner: &DatabaseConnection) -> Vec<EmailEvent> {
    owner
        .query_all(kabipay_organization_tenant::statement(
            "SELECT event FROM important_email_intent ORDER BY created_at,id",
            vec![],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_value(r.try_get("", "event").unwrap()).unwrap())
        .collect()
}
fn authority(user: u32, employee: u32) -> WorkflowApprovalAuthority {
    WorkflowApprovalAuthority {
        actor_user_id: id(user),
        actor_employee: Some(ClientViewerEmployee {
            employee_id: id(employee),
            department_id: None,
        }),
        scope: ScopeType::All,
        permission: "timesheet:approve",
    }
}
async fn draft(owner: &DatabaseConnection, record: u32, employee: u32, date: NaiveDate) {
    owner.execute(kabipay_organization_tenant::statement("INSERT INTO timesheet_entry(id,tenant_id,employee_id,work_date,hours_worked,status,is_deleted,project_code,description) VALUES($1,$2,$3,$4,8,'DRAFT',false,'SYNTHETIC','[task:FIXTURE]')",vec![id(record).into(),id(1).into(),id(employee).into(),date.into()])).await.unwrap();
}
#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_timesheet_submission_decisions_rollback_replay_and_resubmit() {
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
    entity(&owner, timesheet_entry::Entity).await;
    entity(&owner, timesheet_week_batch::Entity).await;
    entity(&owner, workflow::Entity).await;
    entity(&owner, workflow_step::Entity).await;
    entity(&owner, workflow_instance::Entity).await;
    entity(&owner, workflow_action::Entity).await;
    entity(&owner, notification::Entity).await;
    entity(&owner, master_data::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_attendance").await;
    let metadata = DatabaseConnection::Disconnected;
    let (week, _) = week_monday_sunday(Utc::now().date_naive());
    draft(&owner, 40, 2, week).await;
    inject_intent_failure(&owner, true).await;
    assert!(
        submit_timesheet_week(&source, &metadata, id(1), id(2), week)
            .await
            .is_err()
    );
    assert!(events(&owner).await.is_empty());
    assert_eq!(count(&owner, "workflow_instance").await, 0);
    inject_intent_failure(&owner, false).await;
    let batch = submit_timesheet_week(&source, &metadata, id(1), id(2), week)
        .await
        .unwrap();
    let first = events(&owner).await;
    assert_eq!(first.len(), 2);
    assert!(
        submit_timesheet_week(&source, &metadata, id(1), id(2), week)
            .await
            .is_err()
    );
    assert_eq!(events(&owner).await, first);
    assert!(
        approve_timesheet_week_batch(
            &source,
            &metadata,
            id(1),
            batch.id,
            id(31),
            &authority(5, 2)
        )
        .await
        .is_err(),
        "even an ALL grant cannot approve its own batch"
    );
    inject_intent_failure(&owner, true).await;
    assert!(approve_timesheet_week_batch(
        &source,
        &metadata,
        id(1),
        batch.id,
        id(31),
        &authority(10, 110)
    )
    .await
    .is_err());
    assert_eq!(count(&owner, "workflow_action").await, 0);
    assert_eq!(events(&owner).await, first);
    inject_intent_failure(&owner, false).await;
    let progress = approve_timesheet_week_batch(
        &source,
        &metadata,
        id(1),
        batch.id,
        id(31),
        &authority(10, 110),
    )
    .await
    .unwrap();
    assert_eq!(progress.status, "PENDING");
    assert_eq!(events(&owner).await.len(), 3);
    assert!(approve_timesheet_week_batch(
        &source,
        &metadata,
        id(1),
        batch.id,
        id(31),
        &authority(10, 110)
    )
    .await
    .is_err());
    assert_eq!(events(&owner).await.len(), 3);
    inject_intent_failure(&owner, true).await;
    assert!(approve_timesheet_week_batch(
        &source,
        &metadata,
        id(1),
        batch.id,
        id(32),
        &authority(11, 111)
    )
    .await
    .is_err());
    assert_eq!(count(&owner, "workflow_action").await, 1);
    inject_intent_failure(&owner, false).await;
    assert_eq!(
        approve_timesheet_week_batch(
            &source,
            &metadata,
            id(1),
            batch.id,
            id(32),
            &authority(11, 111)
        )
        .await
        .unwrap()
        .status,
        "APPROVED"
    );
    assert_eq!(events(&owner).await.len(), 4);
    draft(&owner, 41, 3, week).await;
    let rejected = submit_timesheet_week(&source, &metadata, id(1), id(3), week)
        .await
        .unwrap();
    let before = events(&owner).await;
    inject_intent_failure(&owner, true).await;
    assert!(reject_timesheet_week_batch(
        &source,
        id(1),
        rejected.id,
        id(31),
        &authority(10, 110),
        Some("Synthetic rejection".into())
    )
    .await
    .is_err());
    assert_eq!(events(&owner).await, before);
    inject_intent_failure(&owner, false).await;
    reject_timesheet_week_batch(
        &source,
        id(1),
        rejected.id,
        id(31),
        &authority(10, 110),
        Some("Synthetic rejection".into()),
    )
    .await
    .unwrap();
    assert_eq!(
        count(&owner, "notification").await,
        1,
        "existing rejection notification remains"
    );
    let rejected_event = events(&owner)
        .await
        .into_iter()
        .find(|event| event.resource_id == rejected.id && event.kind == EmailEventKind::Outcome)
        .unwrap();
    let check = EmailEligibilityRequest {
        intent_id: id(90),
        event_hash: rejected_event.canonical_hash().unwrap(),
        event: rejected_event,
    };
    assert_eq!(
        crate::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    let resubmitted = submit_timesheet_week(&source, &metadata, id(1), id(3), week)
        .await
        .unwrap();
    assert_eq!(resubmitted.id, rejected.id);
    assert_ne!(
        resubmitted.workflow_instance_id,
        rejected.workflow_instance_id
    );
    assert_eq!(
        crate::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    assert!(events(&owner)
        .await
        .iter()
        .all(|event| !serde_json::to_string(event)
            .unwrap()
            .contains("Synthetic rejection")));
}
