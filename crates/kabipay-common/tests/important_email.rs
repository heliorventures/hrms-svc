use chrono::{Duration, Utc};
use kabipay_common::important_email::{
    append_email_intent, stable_event_id, AppendIntentResult, EmailActionTarget,
    EmailAuthorizationRef, EmailCategory, EmailEvent, EmailEventKind, EmailRecipientRef,
    EmailResourceType,
};
use sea_orm::{
    entity::prelude::async_trait, Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
    ProxyRow, Statement, TransactionTrait,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[test]
fn persistence_secret_bytes_have_only_redacted_debug_output() {
    use kabipay_db_entities::tenant::d0132_tenant_email::SecretBytes;
    let secret = SecretBytes(vec![100, 101, 102]);
    assert_eq!(format!("{secret:?}"), "[redacted]");
}

fn event() -> EmailEvent {
    let now = Utc::now();
    let resource_id = Uuid::new_v4();
    EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Leave,
        resource_id,
        resource_version: "decision-receipt-v1".into(),
        category: EmailCategory::Leave,
        kind: EmailEventKind::Outcome,
        template_version: 1,
        recipient: EmailRecipientRef::LocalUser {
            user_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "approved".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id,
        },
        occurred_at: now,
        expires_at: now + Duration::days(7),
    }
}

#[test]
fn typed_events_validate_domain_category_identity_and_expiry() {
    let mut value = event();
    // Targets must bind the same resource; route rendering never accepts a URL.
    value.action_target = EmailActionTarget::CompanyResource {
        resource_type: value.resource_type,
        resource_id: value.resource_id,
    };
    assert!(value.validate().is_ok());
    value.category = EmailCategory::Approval;
    assert!(value.validate().is_err());
    value.category = EmailCategory::Leave;
    value.tenant_id = Uuid::nil();
    assert!(value.validate().is_err());
}

#[test]
fn event_revision_identity_is_stable_and_action_expiry_is_bounded() {
    let value = event();
    let id = stable_event_id(
        value.tenant_id,
        value.resource_type,
        value.resource_id,
        &value.resource_version,
        value.kind,
    );
    assert_eq!(
        id,
        stable_event_id(
            value.tenant_id,
            value.resource_type,
            value.resource_id,
            &value.resource_version,
            value.kind
        )
    );
    assert_ne!(
        id,
        stable_event_id(
            value.tenant_id,
            value.resource_type,
            value.resource_id,
            "different-revision",
            value.kind
        )
    );
    let mut action = value;
    action.kind = EmailEventKind::ActionRequired;
    assert!(
        action.validate().is_err(),
        "action alerts cannot remain queued for seven days"
    );
}

#[derive(Clone, Debug)]
struct Replies {
    rows: Arc<Mutex<VecDeque<Vec<ProxyRow>>>>,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Replies {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        self.rows
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(DbErr::Custom("unexpected query".into()))
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("unexpected standalone write".into()))
    }
}

#[tokio::test]
async fn writer_uses_supplied_transaction_and_compares_immutable_acknowledgement() {
    let mut value = event();
    value.action_target = EmailActionTarget::CompanyResource {
        resource_type: value.resource_type,
        resource_id: value.resource_id,
    };
    let hash = value.canonical_hash().unwrap();
    let acknowledgement = ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::new_v4().into()),
        ("event_hash".into(), hash.into()),
    ]));
    let replies = Replies {
        rows: Arc::new(Mutex::new(VecDeque::from([vec![], vec![acknowledgement]]))),
        queries: Arc::new(Mutex::new(vec![])),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(replies.clone())))
        .await
        .unwrap();
    let tx = db.begin().await.unwrap();
    assert_eq!(
        append_email_intent(&tx, &value).await.unwrap(),
        AppendIntentResult::ExistingSame
    );
    tx.rollback().await.unwrap();
    let queries = replies.queries.lock().unwrap();
    assert_eq!(queries.len(), 2);
    assert!(queries[0].contains("ON CONFLICT(tenant_id,event_id,recipient_key) DO NOTHING"));
    assert!(queries[1].contains(&value.tenant_id.to_string()));
    assert!(!queries
        .iter()
        .any(|sql| sql.contains("tenant_email_settings")));
}

#[tokio::test]
async fn current_email_authority_uses_exact_live_permission_scope_without_role_names() {
    use kabipay_common::{context::ScopeType, important_email::current_email_permission_scope};
    let replies = Replies {
        rows: Arc::new(Mutex::new(VecDeque::from([
            vec![
                ProxyRow::new(BTreeMap::from([("scope_type".into(), "TEAM".into())])),
                ProxyRow::new(BTreeMap::from([("scope_type".into(), "ALL".into())])),
            ],
            vec![],
        ]))),
        queries: Arc::new(Mutex::new(vec![])),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(replies.clone())))
        .await
        .unwrap();
    let tenant = Uuid::new_v4();
    let user = Uuid::new_v4();
    assert_eq!(
        current_email_permission_scope(&db, tenant, user, "leave:approve")
            .await
            .unwrap(),
        Some(ScopeType::All)
    );
    assert_eq!(
        current_email_permission_scope(&db, tenant, user, "leave:approve")
            .await
            .unwrap(),
        None,
        "revoked grant is not cached"
    );
    assert!(
        current_email_permission_scope(&db, tenant, user, "leave:approve:extra")
            .await
            .is_err()
    );
    let queries = replies.queries.lock().unwrap();
    assert_eq!(queries.len(), 2);
    for sql in queries.iter() {
        assert!(sql.contains("role_permission"));
        assert!(sql.contains("permission_scope"));
        assert!(sql.contains("r.tenant_id"));
        assert!(sql.contains("ps.tenant_id"));
        assert!(sql.contains("NOT r.is_deleted"));
        assert!(sql.contains("u.is_active"));
        assert!(!sql.contains("r.name"));
    }
}

#[test]
fn requester_outcome_survives_finalization_but_resolved_action_is_obsolete() {
    use kabipay_common::important_email::{
        check_email_subject, EmailSubjectCheck, EmailSubjectSnapshot,
    };
    let mut value = event();
    let user = match value.recipient {
        EmailRecipientRef::LocalUser { user_id } => user_id,
        _ => unreachable!(),
    };
    let mut subject = EmailSubjectSnapshot {
        employee_id: Uuid::new_v4(),
        requester_user_id: Some(user),
        resource_version: "new-final-version".into(),
        pending: false,
        outcome_is_current: true,
    };
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::RequesterOutcome
    );
    subject.outcome_is_current = false;
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::Obsolete,
        "a correction/reversal supersedes the old outcome"
    );
    subject.outcome_is_current = true;
    value.kind = EmailEventKind::ActionRequired;
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::Obsolete
    );
    subject.pending = true;
    subject.resource_version = value.resource_version.clone();
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::Forbidden,
        "self approval remains forbidden"
    );
    value.recipient = EmailRecipientRef::LocalUser {
        user_id: Uuid::new_v4(),
    };
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::CheckCurrentActionAuthority
    );
    subject.resource_version = "changed".into();
    assert_eq!(
        check_email_subject(&value, &subject),
        EmailSubjectCheck::Obsolete
    );
}

#[test]
fn organization_relationship_is_a_first_class_email_authority_reference() {
    let id = Uuid::new_v4();
    let value = serde_json::json!({"type":"organization_relationship","organization_id":id,"membership_id":id,"relationship_id":id,"version":1});
    let authority: EmailAuthorizationRef = serde_json::from_value(value.clone())
        .expect("relationship must not masquerade as a role binding");
    assert_eq!(serde_json::to_value(authority).unwrap(), value);
}

#[test]
fn mail_uses_the_existing_configured_workflow_relationship_rule() {
    use kabipay_common::workflow_approval::assert_workflow_step_relationship_for_permission;
    use kabipay_db_entities::tenant::d0025_workflow::workflow_step;
    let now = Utc::now();
    let mut step = workflow_step::Model {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        workflow_id: Uuid::new_v4(),
        sequence_order: 1,
        step_name: "Review".into(),
        approver_type: Some("REPORTING_MANAGER".into()),
        approver_role_id: None,
        approver_permission: Some("leave:approve".into()),
        can_skip: false,
        sla_hours: None,
        created_at: now,
        updated_at: now,
    };
    let no_manager = || Err(kabipay_common::KabiPayError::Unauthorised);
    assert!(
        assert_workflow_step_relationship_for_permission(&step, "leave:approve", no_manager())
            .is_err()
    );
    step.approver_type = Some("REPORTING_MANAGER_OR_PERMISSION".into());
    assert!(
        assert_workflow_step_relationship_for_permission(&step, "leave:approve", no_manager())
            .is_ok()
    );
    assert!(assert_workflow_step_relationship_for_permission(
        &step,
        "expense:approve",
        no_manager()
    )
    .is_err());
    step.approver_type = Some("ROLE".into());
    assert!(
        assert_workflow_step_relationship_for_permission(&step, "leave:approve", Ok(())).is_err()
    );
}

#[test]
fn workflow_email_version_changes_when_only_the_pending_step_advances() {
    use kabipay_common::important_email::email_workflow_version;
    let instance = Uuid::new_v4();
    let step = Uuid::new_v4();
    let now = Utc::now();
    let first = email_workflow_version("PENDING:123", instance, step, now);
    assert_eq!(
        first,
        email_workflow_version("PENDING:123", instance, step, now)
    );
    assert_ne!(
        first,
        email_workflow_version("PENDING:123", instance, Uuid::new_v4(), now)
    );
    assert_ne!(
        first,
        email_workflow_version("PENDING:123", instance, step, now + Duration::seconds(1))
    );
    assert!(first.len() <= 256);
}

#[tokio::test]
async fn shared_workflow_scope_reads_relationship_identity_without_profile_values() {
    use kabipay_common::{
        context::ScopeType,
        workflow_approval::{assert_subject_in_approval_scope, WorkflowApprovalAuthority},
    };
    let id = Uuid::new_v4();
    let replies = Replies {
        rows: Arc::new(Mutex::new(VecDeque::from([vec![ProxyRow::new(
            BTreeMap::from([
                ("id".into(), id.into()),
                ("reporting_manager_id".into(), Option::<Uuid>::None.into()),
                ("user_id".into(), Option::<Uuid>::None.into()),
            ]),
        )]]))),
        queries: Arc::new(Mutex::new(vec![])),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(replies.clone())))
        .await
        .unwrap();
    assert_subject_in_approval_scope(
        &db,
        Uuid::new_v4(),
        id,
        &WorkflowApprovalAuthority {
            actor_user_id: Uuid::new_v4(),
            actor_employee: None,
            scope: ScopeType::All,
            permission: "leave:approve",
        },
    )
    .await
    .unwrap();
    let queries = replies.queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(!queries[0].contains("date_of_birth"));
    assert!(!queries[0].contains("personal_phone"));
    assert!(!queries[0].contains("emergency_contact"));
}

#[test]
fn mail_entitlements_follow_the_resource_in_a_shared_runtime() {
    let mut value = event();
    for (resource, permission) in [
        (EmailResourceType::Leave, "leave:read"),
        (EmailResourceType::Expense, "expense:read"),
        (EmailResourceType::Travel, "travel:read"),
        (EmailResourceType::TaxProof, "tax:read"),
        (EmailResourceType::Timesheet, "timesheet:read"),
        (EmailResourceType::ProfileChange, "employee:read"),
        (EmailResourceType::LoanRequest, "loan:read"),
        (EmailResourceType::Payslip, "payroll:read"),
        (EmailResourceType::PerformanceReview, "performance:evaluate"),
        (EmailResourceType::Attendance, "attendance:read"),
        (EmailResourceType::Test, "email_settings:read"),
    ] {
        value.resource_type = resource;
        assert_eq!(
            value.required_module(),
            kabipay_common::entitlements::permission_module(permission)
        );
    }
}

#[tokio::test]
async fn mail_workflow_eligibility_never_takes_update_requiring_assignment_locks() {
    use kabipay_common::{
        context::{ClientViewerEmployee, ScopeType},
        workflow_approval::{assert_workflow_step_actor_read_only, WorkflowApprovalAuthority},
    };
    use kabipay_db_entities::tenant::d0025_workflow::workflow_step;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let manager = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let now = Utc::now();
    let subject_row = || {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), subject.into()),
            ("reporting_manager_id".into(), Some(manager).into()),
            ("user_id".into(), Option::<Uuid>::None.into()),
        ]))
    };
    let manager_row = ProxyRow::new(BTreeMap::from([
        ("id".into(), manager.into()),
        ("reporting_manager_id".into(), Option::<Uuid>::None.into()),
        ("user_id".into(), Some(actor).into()),
    ]));
    let replies = Replies {
        rows: Arc::new(Mutex::new(VecDeque::from([
            vec![subject_row()],
            vec![subject_row()],
            vec![ProxyRow::new(BTreeMap::from([(
                "manager_employee_id".into(),
                Some(manager).into(),
            )]))],
            vec![manager_row],
        ]))),
        queries: Arc::new(Mutex::new(vec![])),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(replies.clone())))
        .await
        .unwrap();
    let step = workflow_step::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        workflow_id: Uuid::new_v4(),
        sequence_order: 1,
        step_name: "Manager".into(),
        approver_type: Some("REPORTING_MANAGER".into()),
        approver_role_id: None,
        approver_permission: Some("leave:approve".into()),
        can_skip: false,
        sla_hours: None,
        created_at: now,
        updated_at: now,
    };
    assert_workflow_step_actor_read_only(
        &db,
        tenant,
        subject,
        &step,
        &WorkflowApprovalAuthority {
            actor_user_id: actor,
            actor_employee: Some(ClientViewerEmployee {
                employee_id: manager,
                department_id: None,
            }),
            scope: ScopeType::All,
            permission: "leave:approve",
        },
    )
    .await
    .unwrap();
    let queries = replies.queries.lock().unwrap();
    assert_eq!(queries.len(), 4);
    assert!(queries[2].contains("management_assignment"));
    assert!(queries
        .iter()
        .all(|query| !query.contains("FOR SHARE") && !query.contains("FOR UPDATE")));
}
