use kabipay_common::{
    context::{is_active_employment_status, ClientClaims, ClientViewerEmployee, ScopeType},
    workflow_approval::{self, WorkflowApprovalAuthority},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0025_workflow::{workflow, workflow_instance, workflow_step},
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use std::future::Future;
use uuid::Uuid;

const STATUS_PENDING: &str = "PENDING";
const WORKFLOW_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

#[derive(Debug, Clone, Copy)]
pub struct ExpenseApprovalAuthority {
    pub actor_user_id: Uuid,
    pub actor_employee_id: Option<Uuid>,
    pub scope: ScopeType,
    pub permission: &'static str,
}

impl ExpenseApprovalAuthority {
    pub fn from_claims(
        claims: &ClientClaims,
        permission: &'static str,
    ) -> KabiPayResult<Self> {
        if !claims.has_any_permission(&[permission]) {
            return Err(KabiPayError::Forbidden(format!(
                "{permission} permission is required"
            )));
        }
        let scope = claims.scope_for_permission(permission).ok_or_else(|| {
            KabiPayError::Forbidden(format!(
                "{permission} permission requires an explicit valid TEAM or ALL scope"
            ))
        })?;
        if !matches!(scope, ScopeType::Team | ScopeType::All) {
            return Err(KabiPayError::Forbidden(format!(
                "{permission} permission requires TEAM or ALL scope"
            )));
        }
        let actor_employee_id = claims.employee_id.ok_or_else(|| {
            KabiPayError::Forbidden(format!(
                "{permission} permission requires a JWT-linked employee approver"
            ))
        })?;
        Ok(Self {
            actor_user_id: claims.sub,
            actor_employee_id: Some(actor_employee_id),
            scope,
            permission,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalSnapshot {
    pub pending_stage: Option<String>,
    pub actionable_step_id: Option<Uuid>,
}

fn approval_decision_not_current() -> KabiPayError {
    KabiPayError::Conflict(
        "approval decision is no longer current; refresh and try again".into(),
    )
}

fn employee_rows_query(
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.is_in([actor_employee_id, subject_employee_id]))
        .order_by_asc(employee::Column::Id)
}

fn employee_rows_for_update_query(
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
) -> sea_orm::Select<employee::Entity> {
    employee_rows_query(tenant_id, actor_employee_id, subject_employee_id).lock_exclusive()
}

fn validate_decision_employees(
    rows: Vec<employee::Model>,
    tenant_id: Uuid,
    authority: &ExpenseApprovalAuthority,
    subject_employee_id: Uuid,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    let actor_employee_id = authority
        .actor_employee_id
        .ok_or_else(approval_decision_not_current)?;
    if actor_employee_id == subject_employee_id {
        return Err(KabiPayError::Forbidden(
            "you cannot approve or reject your own request".into(),
        ));
    }
    let actor = rows
        .iter()
        .find(|row| row.id == actor_employee_id)
        .cloned()
        .ok_or_else(approval_decision_not_current)?;
    let subject = rows
        .into_iter()
        .find(|row| row.id == subject_employee_id)
        .ok_or_else(approval_decision_not_current)?;
    let actor_is_valid = actor.tenant_id == tenant_id
        && !actor.is_deleted
        && is_active_employment_status(&actor.status)
        && actor.user_id == Some(authority.actor_user_id);
    let subject_is_valid = subject.tenant_id == tenant_id
        && !subject.is_deleted
        && is_active_employment_status(&subject.status);
    if !actor_is_valid || !subject_is_valid {
        return Err(approval_decision_not_current());
    }
    Ok((
        ClientViewerEmployee {
            employee_id: actor.id,
            department_id: actor.department_id,
        },
        subject,
    ))
}

pub async fn lock_and_validate_decision_employees(
    txn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    authority: &ExpenseApprovalAuthority,
    subject_employee_id: Uuid,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    let actor_employee_id = authority
        .actor_employee_id
        .ok_or_else(approval_decision_not_current)?;
    let rows = employee_rows_for_update_query(tenant_id, actor_employee_id, subject_employee_id)
        .all(txn)
        .await?;
    validate_decision_employees(rows, tenant_id, authority, subject_employee_id)
}

fn workflow_instance_query(
    instance_id: Uuid,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
) -> sea_orm::Select<workflow_instance::Entity> {
    workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(entity_type))
        .filter(workflow_instance::Column::EntityId.eq(entity_id))
        .filter(workflow_instance::Column::Status.eq(WORKFLOW_STATUS_IN_PROGRESS))
}

fn workflow_query(
    workflow_id: Uuid,
    tenant_id: Uuid,
    entity_type: &str,
) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find_by_id(workflow_id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(entity_type))
}

fn workflow_step_query(
    step_id: Uuid,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> sea_orm::Select<workflow_step::Entity> {
    workflow_step::Entity::find_by_id(step_id)
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
}

pub async fn resolve_approval_snapshot(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    entity_type: &'static str,
    entity_id: Uuid,
    status: &str,
    subject_employee_id: Uuid,
    workflow_instance_id: Option<Uuid>,
    authority: Option<&ExpenseApprovalAuthority>,
) -> KabiPayResult<ApprovalSnapshot> {
    if !status.trim().eq_ignore_ascii_case(STATUS_PENDING) {
        return Ok(ApprovalSnapshot::default());
    }
    let Some(instance_id) = workflow_instance_id else {
        return Ok(ApprovalSnapshot::default());
    };
    let Some(instance) = workflow_instance_query(instance_id, tenant_id, entity_type, entity_id)
        .one(db)
        .await?
    else {
        return Ok(ApprovalSnapshot::default());
    };
    let Some(_workflow) = workflow_query(instance.workflow_id, tenant_id, entity_type)
        .one(db)
        .await?
    else {
        return Ok(ApprovalSnapshot::default());
    };
    let Some(current_step_id) = instance.current_step_id else {
        return Ok(ApprovalSnapshot::default());
    };
    let Some(step) = workflow_step_query(current_step_id, tenant_id, instance.workflow_id)
        .one(db)
        .await?
    else {
        return Ok(ApprovalSnapshot::default());
    };
    let mut snapshot = ApprovalSnapshot {
        pending_stage: Some(step.step_name.clone()),
        actionable_step_id: None,
    };
    let Some(authority) = authority else {
        return Ok(snapshot);
    };
    let Some(actor_employee_id) = authority.actor_employee_id else {
        return Ok(snapshot);
    };
    if actor_employee_id == subject_employee_id {
        return Ok(snapshot);
    }
    let rows = employee_rows_query(tenant_id, actor_employee_id, subject_employee_id)
        .all(db)
        .await?;
    let Ok((actor, _subject)) =
        validate_decision_employees(rows, tenant_id, authority, subject_employee_id)
    else {
        return Ok(snapshot);
    };
    let workflow_authority = WorkflowApprovalAuthority {
        actor_user_id: authority.actor_user_id,
        actor_employee: Some(actor),
        scope: authority.scope,
        permission: authority.permission,
    };
    if workflow_approval::assert_workflow_step_actor(
        db,
        tenant_id,
        subject_employee_id,
        &step,
        &workflow_authority,
    )
    .await
    .is_ok()
    {
        snapshot.actionable_step_id = Some(step.id);
    }
    Ok(snapshot)
}

pub async fn lock_current_workflow(
    txn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    entity_type: &'static str,
    entity_id: Uuid,
    workflow_instance_id: Option<Uuid>,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<(workflow_instance::Model, workflow_step::Model)> {
    let instance_id = require_workflow_instance_id(workflow_instance_id)?;
    let instance = workflow_instance_query(instance_id, tenant_id, entity_type, entity_id)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(approval_decision_not_current)?;
    require_expected_workflow_step(instance.current_step_id, expected_workflow_step_id)?;
    workflow_query(instance.workflow_id, tenant_id, entity_type)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(approval_decision_not_current)?;
    let step = workflow_step_query(expected_workflow_step_id, tenant_id, instance.workflow_id)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(approval_decision_not_current)?;
    Ok((instance, step))
}

fn require_workflow_instance_id(workflow_instance_id: Option<Uuid>) -> KabiPayResult<Uuid> {
    workflow_instance_id.ok_or_else(approval_decision_not_current)
}

fn require_expected_workflow_step(
    current_step_id: Option<Uuid>,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<()> {
    if current_step_id == Some(expected_workflow_step_id) {
        Ok(())
    } else {
        Err(approval_decision_not_current())
    }
}

pub async fn after_successful_commit<T, CommitFuture, AfterCommit, AfterCommitFuture>(
    commit: CommitFuture,
    after_commit: AfterCommit,
) -> KabiPayResult<T>
where
    CommitFuture: Future<Output = Result<(), DbErr>>,
    AfterCommit: FnOnce() -> AfterCommitFuture,
    AfterCommitFuture: Future<Output = KabiPayResult<T>>,
{
    commit.await?;
    after_commit().await
}

pub fn workflow_authority(
    authority: &ExpenseApprovalAuthority,
    actor: ClientViewerEmployee,
) -> WorkflowApprovalAuthority {
    WorkflowApprovalAuthority {
        actor_user_id: authority.actor_user_id,
        actor_employee: Some(actor),
        scope: authority.scope,
        permission: authority.permission,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use kabipay_common::context::{
        CLIENT_JWT_ISSUER, PERM_EXPENSE_APPROVE, PERM_TRAVEL_APPROVE,
    };
    use sea_orm::{DbBackend, QueryTrait};
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    fn claims(permission: &str) -> ClientClaims {
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes: HashMap::from([(permission.into(), "TEAM".into())]),
            resource_scopes: HashMap::new(),
        }
    }

    fn employee_row(
        id: Uuid,
        tenant_id: Uuid,
        user_id: Option<Uuid>,
        status: &str,
    ) -> employee::Model {
        employee::Model {
            id,
            tenant_id,
            user_id,
            department_id: None,
            designation_id: None,
            cost_center_id: None,
            location_id: None,
            reporting_manager_id: None,
            employee_code: format!("EMP-{id}"),
            first_name: "Test".into(),
            last_name: "Employee".into(),
            date_of_birth: None,
            gender: None,
            blood_group: None,
            nationality: None,
            employment_type: None,
            status: status.into(),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
            probation_end_date: None,
            notice_period_days: None,
            emergency_contact_name: None,
            emergency_contact_phone: None,
            emergency_contact_relation: None,
            personal_phone: None,
            current_address: None,
            permanent_address: None,
            uan_number: None,
            esic_number: None,
            is_deleted: false,
            deleted_at: None,
            deleted_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn role_name_does_not_create_expense_approval_authority() {
        let mut role_only = claims(PERM_EXPENSE_APPROVE);
        role_only.roles = vec!["HR_ADMIN".into()];
        role_only.permissions.clear();
        role_only.permission_scopes.clear();
        assert!(ExpenseApprovalAuthority::from_claims(&role_only, PERM_EXPENSE_APPROVE).is_err());
    }

    #[test]
    fn permission_and_action_scope_create_expense_approval_authority() {
        let authority = ExpenseApprovalAuthority::from_claims(
            &claims(PERM_EXPENSE_APPROVE),
            PERM_EXPENSE_APPROVE,
        )
        .expect("explicit approval permission");
        assert_eq!(authority.scope, ScopeType::Team);
        assert_eq!(authority.permission, PERM_EXPENSE_APPROVE);
    }

    #[test]
    fn expense_and_travel_approval_permissions_do_not_substitute_for_each_other() {
        assert!(ExpenseApprovalAuthority::from_claims(
            &claims(PERM_EXPENSE_APPROVE),
            PERM_TRAVEL_APPROVE,
        )
        .is_err());
        assert!(ExpenseApprovalAuthority::from_claims(
            &claims(PERM_TRAVEL_APPROVE),
            PERM_EXPENSE_APPROVE,
        )
        .is_err());
    }

    #[test]
    fn approval_permission_without_a_valid_exact_scope_is_rejected() {
        let mut missing_scope = claims(PERM_EXPENSE_APPROVE);
        missing_scope.permission_scopes.clear();
        assert!(ExpenseApprovalAuthority::from_claims(
            &missing_scope,
            PERM_EXPENSE_APPROVE,
        )
        .is_err());
        let mut malformed_scope = claims(PERM_EXPENSE_APPROVE);
        malformed_scope
            .permission_scopes
            .insert(PERM_EXPENSE_APPROVE.into(), "INVALID".into());
        assert!(ExpenseApprovalAuthority::from_claims(
            &malformed_scope,
            PERM_EXPENSE_APPROVE,
        )
        .is_err());
    }

    #[test]
    fn team_and_all_approval_scopes_exclude_the_actors_own_request() {
        for scope in ["TEAM", "ALL"] {
            let mut scoped_claims = claims(PERM_EXPENSE_APPROVE);
            scoped_claims
                .permission_scopes
                .insert(PERM_EXPENSE_APPROVE.into(), scope.into());
            let authority = ExpenseApprovalAuthority::from_claims(
                &scoped_claims,
                PERM_EXPENSE_APPROVE,
            )
            .expect("valid exact approval scope");
            let actor_employee_id = authority.actor_employee_id.expect("linked employee");
            let error = validate_decision_employees(
                Vec::new(),
                scoped_claims.tenant_id,
                &authority,
                actor_employee_id,
            )
            .expect_err("ALL and TEAM must reject self-approval before database scope reads");
            assert!(matches!(error, KabiPayError::Forbidden(_)), "scope={scope}");
        }
    }

    #[test]
    fn stale_duplicate_and_workflowless_decisions_fail_closed() {
        let expected_step_id = Uuid::new_v4();
        assert!(require_expected_workflow_step(Some(expected_step_id), expected_step_id).is_ok());
        for current in [None, Some(Uuid::new_v4())] {
            let error = require_expected_workflow_step(current, expected_step_id)
                .expect_err("workflowless, stale, and duplicate retries must fail");
            assert_eq!(error.code(), "CONFLICT");
        }
        let error = require_workflow_instance_id(None)
            .expect_err("workflowless approval must fail closed");
        assert_eq!(error.code(), "CONFLICT");
    }

    #[test]
    fn decision_queries_lock_employees_instance_workflow_and_expected_step() {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let entity_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();

        let employee_sql = employee_rows_for_update_query(tenant_id, actor_id, subject_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(employee_sql.contains("ORDER BY \"employee\".\"id\" ASC"));
        assert!(employee_sql.ends_with("FOR UPDATE"));

        for entity_type in ["EXPENSE", "TRAVEL_REQUEST"] {
            let instance_sql = workflow_instance_query(
                instance_id,
                tenant_id,
                entity_type,
                entity_id,
            )
            .lock_exclusive()
            .build(DbBackend::Postgres)
            .to_string();
            assert!(instance_sql.contains(entity_type));
            assert!(instance_sql.contains(&entity_id.to_string()));
            assert!(instance_sql.contains("\"status\" = 'IN_PROGRESS'"));
            assert!(instance_sql.ends_with("FOR UPDATE"));

            let workflow_sql = workflow_query(workflow_id, tenant_id, entity_type)
                .lock_exclusive()
                .build(DbBackend::Postgres)
                .to_string();
            assert!(workflow_sql.contains(entity_type));
            assert!(workflow_sql.ends_with("FOR UPDATE"));
        }

        let step_sql = workflow_step_query(step_id, tenant_id, workflow_id)
            .lock_exclusive()
            .build(DbBackend::Postgres)
            .to_string();
        assert!(step_sql.contains(&workflow_id.to_string()));
        assert!(step_sql.ends_with("FOR UPDATE"));
    }

    #[test]
    fn locked_actor_and_subject_must_be_active_linked_and_distinct() {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let actor_user_id = Uuid::new_v4();
        let authority = ExpenseApprovalAuthority {
            actor_user_id,
            actor_employee_id: Some(actor_id),
            scope: ScopeType::All,
            permission: PERM_EXPENSE_APPROVE,
        };
        let rows = vec![
            employee_row(subject_id, tenant_id, Some(Uuid::new_v4()), "ACTIVE"),
            employee_row(actor_id, tenant_id, Some(actor_user_id), "PROBATION"),
        ];
        assert!(validate_decision_employees(rows.clone(), tenant_id, &authority, subject_id)
            .is_ok());

        let mut inactive_actor = rows.clone();
        inactive_actor
            .iter_mut()
            .find(|row| row.id == actor_id)
            .expect("actor")
            .status = "INACTIVE".into();
        assert!(validate_decision_employees(
            inactive_actor,
            tenant_id,
            &authority,
            subject_id,
        )
        .is_err());

        let mut deleted_subject = rows;
        deleted_subject
            .iter_mut()
            .find(|row| row.id == subject_id)
            .expect("subject")
            .is_deleted = true;
        assert!(validate_decision_employees(
            deleted_subject,
            tenant_id,
            &authority,
            subject_id,
        )
        .is_err());
    }

    #[tokio::test]
    async fn expense_and_travel_notifications_run_only_after_successful_commit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let failed_calls = Arc::clone(&calls);
        let failed = after_successful_commit(
            async { Err(DbErr::Custom("commit failed".into())) },
            || async move {
                failed_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, KabiPayError>(())
            },
        )
        .await;
        assert!(failed.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let committed_calls = Arc::clone(&calls);
        after_successful_commit(
            async { Ok(()) },
            || async move {
                committed_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, KabiPayError>(())
            },
        )
        .await
        .expect("post-commit work succeeds");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
