//! Employee queries and write operations on a tenant-scoped connection.
//!
//! Every query applies both the `tenant_id` filter (Gap A — defence in depth even with
//! schema isolation) and the `is_deleted = false` filter (Gap B — soft-delete policy).

use chrono::{NaiveDate, Utc};
use kabipay_common::client_data_scope::{
    resolve_employee_scope_filter, EmployeeScopeFilter,
};
use kabipay_common::context::{
    canonical_employment_status, is_active_employment_status, ClientViewerEmployee, ScopeType,
};
use kabipay_common::db_constraint::constraint_name;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{user, user_role, user_session};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::entities::d0007_employee_core::employee;
use crate::services::rbac_admin_service;

fn employee_conflict_for_constraint(constraint: &str) -> Option<KabiPayError> {
    let (code, message) = match constraint {
        "uq_user_tenant_email" => (
            "USER_EMAIL_CONFLICT",
            "email is already in use in this tenant",
        ),
        "uq_user_tenant_username" => (
            "USER_USERNAME_CONFLICT",
            "username is already in use in this tenant",
        ),
        "uq_employee_tenant_code" => (
            "EMPLOYEE_CODE_CONFLICT",
            "employee code is already in use in this tenant",
        ),
        _ => return None,
    };

    Some(KabiPayError::ConflictRule {
        code,
        message: message.into(),
    })
}

fn map_employee_db_error(error: sea_orm::DbErr) -> KabiPayError {
    constraint_name(&error)
        .and_then(employee_conflict_for_constraint)
        .unwrap_or(KabiPayError::Database(error))
}

#[cfg(test)]
mod constraint_mapping_tests {
    use super::*;

    #[test]
    fn known_employee_identity_constraints_have_stable_public_codes() {
        assert_eq!(
            employee_conflict_for_constraint("uq_user_tenant_email")
                .unwrap()
                .code(),
            "USER_EMAIL_CONFLICT"
        );
        assert_eq!(
            employee_conflict_for_constraint("uq_user_tenant_username")
                .unwrap()
                .code(),
            "USER_USERNAME_CONFLICT"
        );
        assert_eq!(
            employee_conflict_for_constraint("uq_employee_tenant_code")
                .unwrap()
                .code(),
            "EMPLOYEE_CODE_CONFLICT"
        );
        assert!(employee_conflict_for_constraint("unknown_constraint").is_none());
    }
}

#[cfg(test)]
mod employee_scope_filter_tests {
    use super::*;
    use kabipay_common::client_data_scope::EmployeeScopeFilter;

    #[test]
    fn employee_id_scope_adapter_preserves_empty_bounded_and_unrestricted_filters() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        assert_eq!(
            employee_ids_from_scope_filter(EmployeeScopeFilter::Empty),
            Some(vec![])
        );
        assert_eq!(
            employee_ids_from_scope_filter(EmployeeScopeFilter::EmployeeIds(vec![first, second])),
            Some(vec![first, second])
        );
        assert_eq!(
            employee_ids_from_scope_filter(EmployeeScopeFilter::Unrestricted),
            None
        );
    }
}

#[cfg(test)]
mod employee_status_boundary_tests {
    use super::*;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
    use std::sync::{Arc, Mutex};

    fn new_employee(status: &str) -> NewEmployee {
        NewEmployee {
            employee_code: "EMP-STATUS-TEST".into(),
            first_name: "Status".into(),
            last_name: "Test".into(),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
            department_id: None,
            designation_id: None,
            reporting_manager_id: None,
            employment_type: Some("FULL_TIME".into()),
            status: status.into(),
            user_id: None,
        }
    }

    fn status_only_patch(status: &str) -> EmployeePatch {
        EmployeePatch {
            first_name: None,
            last_name: None,
            department_id: None,
            designation_id: None,
            reporting_manager_id: None,
            employment_type: None,
            status: Some(status.into()),
            user_id: None,
            linked_user_email: None,
        }
    }

    #[derive(Debug)]
    struct StatementRecorder {
        statements: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for StatementRecorder {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("statement recorder")
                .push(format!("{statement}"));
            Ok(Vec::new())
        }

        async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
            self.statements
                .lock()
                .expect("statement recorder")
                .push(format!("{statement}"));
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
        }
    }

    #[tokio::test]
    async fn create_rejects_unknown_status_before_database_access() {
        let error = create(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            new_employee("NOTICE"),
        )
        .await
        .expect_err("unknown status must be rejected at the create boundary");
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn update_rejects_empty_status_before_transaction_or_lookup() {
        let error = update(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            Uuid::new_v4(),
            status_only_patch("   "),
        )
        .await
        .expect_err("empty status must be rejected at the update boundary");
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn create_normalizes_supported_status_before_insert() {
        let statements = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(StatementRecorder {
                statements: Arc::clone(&statements),
            })),
        )
        .await
        .expect("proxy database");

        let _ = create(&db, Uuid::new_v4(), new_employee(" probation ")).await;

        let statements = statements.lock().expect("statement recorder");
        let insert = statements
            .iter()
            .find(|statement| statement.contains("INSERT INTO \"employee\""))
            .expect("employee insert statement");
        assert!(insert.contains("'PROBATION'"), "insert={insert}");
        assert!(!insert.contains("' probation '"), "insert={insert}");
    }
}

#[cfg(test)]
mod login_role_integrity_tests {
    use super::*;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ScriptedProxy {
        query_results: Mutex<VecDeque<Result<Vec<ProxyRow>, DbErr>>>,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ScriptedProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("QUERY {statement}"));
            self.query_results
                .lock()
                .expect("query script")
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("EXECUTE {statement}"));
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
        }
    }

    async fn scripted_connection(
        query_results: Vec<Result<Vec<ProxyRow>, DbErr>>,
    ) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(ScriptedProxy {
                query_results: Mutex::new(query_results.into()),
                events: Arc::clone(&events),
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (db, events)
    }

    fn user_row(user_id: Uuid, tenant_id: Uuid, is_active: bool) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), user_id.into()),
            ("tenant_id".into(), tenant_id.into()),
            ("username".into(), "role-test-user".to_string().into()),
            ("email".into(), Option::<String>::None.into()),
            ("password_hash".into(), "hash".to_string().into()),
            ("must_change_password".into(), false.into()),
            ("is_active".into(), is_active.into()),
            ("mfa_enabled".into(), false.into()),
            ("mfa_secret".into(), Option::<String>::None.into()),
            ("last_login_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("is_deleted".into(), false.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))
    }

    fn role_row(role_id: Uuid, tenant_id: Uuid, is_deleted: bool) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), role_id.into()),
            ("tenant_id".into(), tenant_id.into()),
            ("name".into(), "EMPLOYEE".to_string().into()),
            ("description".into(), Option::<String>::None.into()),
            ("is_system_role".into(), true.into()),
            ("is_deleted".into(), is_deleted.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))
    }

    fn login_account(role_ids: Vec<Uuid>) -> NewLoginAccount {
        NewLoginAccount {
            username: "role-test-user".into(),
            email: None,
            password_hash: "hash".into(),
            role_ids,
        }
    }

    fn active_employee() -> NewEmployee {
        NewEmployee {
            employee_code: "EMP-ROLE-TEST".into(),
            first_name: "Role".into(),
            last_name: "Test".into(),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
            department_id: None,
            designation_id: None,
            reporting_manager_id: None,
            employment_type: Some("FULL_TIME".into()),
            status: "ACTIVE".into(),
            user_id: None,
        }
    }

    #[tokio::test]
    async fn create_with_login_rejects_empty_roles_before_opening_a_transaction() {
        let error = create_with_login(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            active_employee(),
            login_account(Vec::new()),
        )
        .await
        .expect_err("login creation requires at least one role");

        assert_eq!(error.code(), "ACTIVE_LOGIN_ROLE_REQUIRED");
    }

    #[tokio::test]
    async fn provision_login_rejects_empty_roles_before_opening_a_transaction() {
        let error = provision_login(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            Uuid::new_v4(),
            login_account(Vec::new()),
        )
        .await
        .expect_err("login provisioning requires at least one role");

        assert_eq!(error.code(), "ACTIVE_LOGIN_ROLE_REQUIRED");
    }

    #[tokio::test]
    async fn create_with_login_rejects_cross_tenant_role_before_account_writes() {
        let tenant_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(vec![Ok(vec![role_row(
            role_id,
            Uuid::new_v4(),
            false,
        )])])
        .await;

        let error = create_with_login(
            &db,
            tenant_id,
            active_employee(),
            login_account(vec![role_id]),
        )
        .await
        .expect_err("cross-tenant role must be rejected");

        assert_eq!(error.code(), "NOT_FOUND");
        let events = events.lock().expect("event recorder");
        assert!(
            !events.iter().any(|event| event.contains("INSERT INTO \"user\"")),
            "events={events:?}"
        );
    }

    #[tokio::test]
    async fn provision_login_rejects_deleted_role_before_employee_or_account_writes() {
        let tenant_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) =
            scripted_connection(vec![Ok(vec![role_row(role_id, tenant_id, true)])]).await;

        let error = provision_login(
            &db,
            tenant_id,
            Uuid::new_v4(),
            login_account(vec![role_id]),
        )
        .await
        .expect_err("deleted role must be rejected");

        assert_eq!(error.code(), "NOT_FOUND");
        let events = events.lock().expect("event recorder");
        assert!(
            !events.iter().any(|event| event.contains("FROM \"employee\"")),
            "events={events:?}"
        );
        assert!(
            !events.iter().any(|event| event.contains("INSERT INTO \"user\"")),
            "events={events:?}"
        );
    }

    #[tokio::test]
    async fn active_employee_linked_login_rejects_zero_active_roles() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, _events) =
            scripted_connection(vec![Ok(vec![user_row(user_id, tenant_id, true)]), Ok(vec![])])
                .await;

        let error = sync_linked_user_status(&db, tenant_id, Some(user_id), "ACTIVE")
            .await
            .expect_err("an active employee-linked login must retain an active role");

        assert_eq!(error.code(), "ACTIVE_LOGIN_ROLE_REQUIRED");
    }

    #[tokio::test]
    async fn deactivation_without_roles_is_allowed_and_revokes_sessions() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, events) = scripted_connection(vec![
            Ok(vec![user_row(user_id, tenant_id, true)]),
            Ok(vec![user_row(user_id, tenant_id, false)]),
        ])
        .await;

        sync_linked_user_status(&db, tenant_id, Some(user_id), "INACTIVE")
            .await
            .expect("canonical deactivation must not require a role assignment");

        let events = events.lock().expect("event recorder");
        assert!(
            events
                .iter()
                .any(|event| event.contains("DELETE FROM \"user_session\"")),
            "events={events:?}"
        );
        assert!(
            !events.iter().any(|event| event.contains("FROM \"user_role\"")),
            "deactivation must not be blocked by role lookup: events={events:?}"
        );
    }

    #[tokio::test]
    async fn linked_user_from_another_tenant_is_rejected() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, _events) = scripted_connection(vec![Ok(Vec::new())]).await;

        let error = sync_linked_user_status(&db, tenant_id, Some(user_id), "ACTIVE")
            .await
            .expect_err("an employee cannot link a user outside the tenant");

        assert_eq!(error.code(), "NOT_FOUND");
    }
}

async fn sync_linked_user_status<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    employee_status: &str,
) -> KabiPayResult<()> {
    let Some(user_id) = user_id else {
        return Ok(());
    };
    let should_be_active = is_active_employment_status(employee_status);
    let found = user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .filter(|row| row.tenant_id == tenant_id && !row.is_deleted)
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        })?;
    if should_be_active {
        rbac_admin_service::require_user_active_tenant_role(db, tenant_id, user_id).await?;
    }
    if found.is_active != should_be_active {
        let mut am: user::ActiveModel = found.into();
        am.is_active = Set(should_be_active);
        am.updated_at = Set(Utc::now());
        am.update(db).await?;
    }
    if !should_be_active {
        user_session::Entity::delete_many()
            .filter(user_session::Column::UserId.eq(user_id))
            .exec(db)
            .await?;
    }
    Ok(())
}

async fn ensure_linked_user_role_integrity<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    employee_status: &str,
) -> KabiPayResult<()> {
    let Some(user_id) = user_id else {
        return Ok(());
    };
    user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .filter(|row| row.tenant_id == tenant_id && !row.is_deleted)
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        })?;
    if is_active_employment_status(employee_status) {
        rbac_admin_service::require_user_active_tenant_role(db, tenant_id, user_id).await?;
    }
    Ok(())
}

async fn update_linked_user_email<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    email: Option<String>,
) -> KabiPayResult<()> {
    let user_id = user_id.ok_or_else(|| {
        KabiPayError::Validation("employee does not have a linked login user".into())
    })?;
    let normalized = normalize_email(email);
    if let Some(ref email_value) = normalized {
        let existing = user::Entity::find()
            .filter(user::Column::TenantId.eq(tenant_id))
            .filter(user::Column::Email.eq(email_value))
            .filter(user::Column::Id.ne(user_id))
            .one(db)
            .await?;
        if existing.is_some() {
            return Err(KabiPayError::ConflictRule {
                code: "USER_EMAIL_CONFLICT",
                message: "email is already in use in this tenant".into(),
            });
        }
    }
    let user_row = user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        })?;
    let mut am: user::ActiveModel = user_row.into();
    am.email = Set(normalized);
    am.updated_at = Set(Utc::now());
    am.update(db).await.map_err(map_employee_db_error)?;
    Ok(())
}

/// `new_manager` must exist, differ from `subject_employee_id`, and must not create a reporting loop.
pub async fn assert_valid_reporting_manager<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    new_manager_id: Uuid,
) -> KabiPayResult<()> {
    if subject_employee_id == new_manager_id {
        return Err(KabiPayError::Validation(
            "an employee cannot report to themselves".into(),
        ));
    }
    find_by_id(db, tenant_id, new_manager_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: new_manager_id.to_string(),
        })?;

    let mut current = new_manager_id;
    for _ in 0..64 {
        let row = find_by_id(db, tenant_id, current)
            .await?
            .ok_or_else(|| KabiPayError::Internal("reporting chain broke".into()))?;
        let Some(mid) = row.reporting_manager_id else {
            break;
        };
        if mid == subject_employee_id {
            return Err(KabiPayError::Validation(
                "that reporting manager would create a loop in the org chart".into(),
            ));
        }
        current = mid;
    }
    Ok(())
}

/// Look up one non-deleted employee inside a tenant schema.
///
/// Returns `Ok(None)` when the employee is not found (or is soft-deleted / belongs
/// to another tenant) so the resolver can render a nullable `Employee`.
pub async fn find_by_id<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Option<employee::Model>> {
    employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

/// Batch-load active tenant employees for review queues and other bounded enrichments.
pub async fn find_by_ids<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    employee_ids: &[Uuid],
) -> KabiPayResult<Vec<employee::Model>> {
    if employee_ids.is_empty() {
        return Ok(Vec::new());
    }
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.is_in(employee_ids.iter().copied()))
        .filter(employee::Column::IsDeleted.eq(false))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Full display names for referenced employees (e.g. reporting manager labels).
pub async fn map_full_names(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows
        .into_iter()
        .map(|m| {
            let full_name = format!("{} {}", m.first_name.trim(), m.last_name.trim())
                .trim()
                .to_string();
            (m.id, full_name)
        })
        .collect())
}

fn employee_ids_from_scope_filter(filter: EmployeeScopeFilter) -> Option<Vec<Uuid>> {
    match filter {
        EmployeeScopeFilter::Unrestricted => None,
        EmployeeScopeFilter::Empty => Some(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) => Some(ids),
    }
}

/// Resolve all employee IDs visible to a caller for cross-record approval queues.
/// `None` means unrestricted tenant scope; `Some([])` means no visible employees.
pub async fn employee_ids_in_scope(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<Option<Vec<Uuid>>> {
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer).await?;
    Ok(employee_ids_from_scope_filter(filter))
}

/// List the first `limit` non-deleted employees, filtered by the caller’s data scope
/// (`ALL` = entire tenant, otherwise `scope` + `viewer`).
///
/// `limit` is clamped to the range `1..=100` so a caller cannot force a full-table scan.
/// When the scope is not `All` and `viewer` is missing (no linked employee), returns an empty list.
pub async fn list(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<Vec<employee::Model>> {
    let limit = limit.clamp(1, 100);
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer).await?;
    let mut q = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false));

    match filter {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) => {
            q = q.filter(employee::Column::Id.is_in(ids));
        }
    }

    q.limit(limit).all(db).await.map_err(KabiPayError::from)
}

/// Employees visible under the same data scope as [`list`], with a higher row cap for org-chart views.
/// `limit` is clamped to `1..=500`.
pub async fn list_for_org_chart(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<Vec<employee::Model>> {
    let limit = limit.clamp(1, 500);
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer).await?;
    let mut q = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false));

    match filter {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) => {
            q = q.filter(employee::Column::Id.is_in(ids));
        }
    }

    q.order_by_asc(employee::Column::EmployeeCode)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Payload for a new `employee` row (no GraphQL types here).
pub struct NewEmployee {
    pub employee_code: String,
    pub first_name: String,
    pub last_name: String,
    pub date_of_joining: NaiveDate,
    pub department_id: Option<Uuid>,
    pub designation_id: Option<Uuid>,
    pub reporting_manager_id: Option<Uuid>,
    pub employment_type: Option<String>,
    pub status: String,
    pub user_id: Option<Uuid>,
}

pub struct NewLoginAccount {
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub role_ids: Vec<Uuid>,
}

fn normalize_username(raw: &str) -> KabiPayResult<String> {
    let username = raw.trim().to_lowercase();
    if username.is_empty() {
        return Err(KabiPayError::Validation("username is required".into()));
    }
    Ok(username)
}

fn normalize_email(raw: Option<String>) -> Option<String> {
    raw.and_then(|value| {
        let trimmed = value.trim().to_lowercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

async fn assign_validated_roles<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    role_ids: &[Uuid],
) -> KabiPayResult<()> {
    let now = Utc::now();
    for role_id in role_ids {
        user_role::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(*role_id),
            assigned_at: Set(now),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

async fn insert_login_user<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    account: NewLoginAccount,
    validated_role_ids: Vec<Uuid>,
    employee_status: &str,
) -> KabiPayResult<Uuid> {
    let username = normalize_username(&account.username)?;
    let email = normalize_email(account.email);
    let password_hash = account.password_hash;
    if user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::Username.eq(&username))
        .one(db)
        .await?
        .is_some()
    {
        return Err(KabiPayError::ConflictRule {
            code: "USER_USERNAME_CONFLICT",
            message: "username is already in use in this tenant".into(),
        });
    }
    if let Some(ref email_value) = email {
        if user::Entity::find()
            .filter(user::Column::TenantId.eq(tenant_id))
            .filter(user::Column::Email.eq(email_value))
            .one(db)
            .await?
            .is_some()
        {
            return Err(KabiPayError::ConflictRule {
                code: "USER_EMAIL_CONFLICT",
                message: "email is already in use in this tenant".into(),
            });
        }
    }

    let id = Uuid::new_v4();
    let now = Utc::now();
    user::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        username: Set(username),
        email: Set(email),
        password_hash: Set(password_hash),
        must_change_password: Set(true),
        is_active: Set(is_active_employment_status(employee_status)),
        mfa_enabled: Set(false),
        mfa_secret: Set(None),
        last_login_at: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(map_employee_db_error)?;
    assign_validated_roles(db, id, &validated_role_ids).await?;
    Ok(id)
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    mut data: NewEmployee,
) -> KabiPayResult<employee::Model> {
    data.status = canonical_employment_status(&data.status)?.to_owned();
    ensure_linked_user_role_integrity(db, tenant_id, data.user_id, &data.status).await?;
    if employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::EmployeeCode.eq(&data.employee_code))
        .one(db)
        .await?
        .is_some()
    {
        return Err(KabiPayError::ConflictRule {
            code: "EMPLOYEE_CODE_CONFLICT",
            message: "employee code is already in use in this tenant".into(),
        });
    }

    let id = Uuid::new_v4();
    if let Some(mgr) = data.reporting_manager_id {
        assert_valid_reporting_manager(db, tenant_id, id, mgr).await?;
    }
    let now = Utc::now();
    let am = employee::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        user_id: Set(data.user_id),
        department_id: Set(data.department_id),
        designation_id: Set(data.designation_id),
        cost_center_id: Set(None),
        location_id: Set(None),
        reporting_manager_id: Set(data.reporting_manager_id),
        employee_code: Set(data.employee_code),
        first_name: Set(data.first_name),
        last_name: Set(data.last_name),
        date_of_birth: Set(None),
        gender: Set(None),
        blood_group: Set(None),
        nationality: Set(None),
        employment_type: Set(data.employment_type),
        status: Set(data.status),
        date_of_joining: Set(data.date_of_joining),
        probation_end_date: Set(None),
        notice_period_days: Set(None),
        emergency_contact_name: Set(None),
        emergency_contact_phone: Set(None),
        emergency_contact_relation: Set(None),
        personal_phone: Set(None),
        current_address: Set(None),
        permanent_address: Set(None),
        uan_number: Set(None),
        esic_number: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(map_employee_db_error)?;
    employee::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted employee not found".into()))
}

/// Partial update: each `Some` field replaces the column; `None` = leave unchanged.
pub struct EmployeePatch {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub department_id: Option<Uuid>,
    pub designation_id: Option<Uuid>,
    /// `None` = do not change; `Some(None)` = clear; `Some(Some(u))` = set (validated).
    pub reporting_manager_id: Option<Option<Uuid>>,
    pub employment_type: Option<String>,
    pub status: Option<String>,
    pub user_id: Option<Uuid>,
    /// Omitted = leave unchanged; Some("") clears the optional linked login email.
    pub linked_user_email: Option<String>,
}

pub async fn create_with_login(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    mut data: NewEmployee,
    account: NewLoginAccount,
) -> KabiPayResult<employee::Model> {
    if data.user_id.is_some() {
        return Err(KabiPayError::Validation(
            "userId cannot be supplied when loginAccount is used".into(),
        ));
    }
    rbac_admin_service::require_nonempty_role_assignment(&account.role_ids)?;
    data.status = canonical_employment_status(&data.status)?.to_owned();
    let txn = db.begin().await?;
    let validated_role_ids =
        rbac_admin_service::validated_active_role_ids(&txn, tenant_id, &account.role_ids).await?;
    let user_id = insert_login_user(
        &txn,
        tenant_id,
        account,
        validated_role_ids,
        &data.status,
    )
    .await?;
    data.user_id = Some(user_id);
    let created = create(&txn, tenant_id, data).await?;
    txn.commit().await?;
    Ok(created)
}

pub async fn provision_login(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    account: NewLoginAccount,
) -> KabiPayResult<employee::Model> {
    rbac_admin_service::require_nonempty_role_assignment(&account.role_ids)?;
    let txn = db.begin().await?;
    let validated_role_ids =
        rbac_admin_service::validated_active_role_ids(&txn, tenant_id, &account.role_ids).await?;
    let existing = find_by_id(&txn, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    if existing.user_id.is_some() {
        return Err(KabiPayError::Conflict(
            "employee already has a linked login user".into(),
        ));
    }
    let user_id = insert_login_user(
        &txn,
        tenant_id,
        account,
        validated_role_ids,
        &existing.status,
    )
    .await?;
    let mut am: employee::ActiveModel = existing.into();
    am.user_id = Set(Some(user_id));
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;
    let updated = find_by_id(&txn, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated employee not found".into()))?;
    txn.commit().await?;
    Ok(updated)
}

pub async fn reset_linked_user_password(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    password_hash: String,
) -> KabiPayResult<()> {
    let txn = db.begin().await?;
    let employee = find_by_id(&txn, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    let user_id = employee.user_id.ok_or_else(|| {
        KabiPayError::Validation("employee does not have a linked login user".into())
    })?;
    let user_row = user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        })?;
    let mut am: user::ActiveModel = user_row.into();
    am.password_hash = Set(password_hash);
    am.must_change_password = Set(true);
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;
    user_session::Entity::delete_many()
        .filter(user_session::Column::UserId.eq(user_id))
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

pub async fn update(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    mut patch: EmployeePatch,
) -> KabiPayResult<employee::Model> {
    if let Some(status) = patch.status.as_mut() {
        *status = canonical_employment_status(status)?.to_owned();
    }
    let txn = db.begin().await?;
    let existing = find_by_id(&txn, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    let mut final_user_id = existing.user_id;
    if let Some(v) = patch.user_id {
        final_user_id = Some(v);
    }

    let mut final_status = existing.status.clone();
    let mut am: employee::ActiveModel = existing.into();
    if let Some(v) = patch.first_name {
        am.first_name = Set(v);
    }
    if let Some(v) = patch.last_name {
        am.last_name = Set(v);
    }
    if let Some(v) = patch.department_id {
        am.department_id = Set(Some(v));
    }
    if let Some(v) = patch.designation_id {
        am.designation_id = Set(Some(v));
    }
    if let Some(inner) = patch.reporting_manager_id {
        match inner {
            None => {
                am.reporting_manager_id = Set(None);
            }
            Some(mgr) => {
                assert_valid_reporting_manager(&txn, tenant_id, employee_id, mgr).await?;
                am.reporting_manager_id = Set(Some(mgr));
            }
        }
    }
    if let Some(v) = patch.employment_type {
        am.employment_type = Set(Some(v));
    }
    if let Some(v) = patch.status {
        final_status = v.clone();
        am.status = Set(v);
    }
    let linked_user_email = patch.linked_user_email;
    am.user_id = Set(final_user_id);
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;
    if let Some(email) = linked_user_email {
        update_linked_user_email(&txn, tenant_id, final_user_id, Some(email)).await?;
    }
    sync_linked_user_status(&txn, tenant_id, final_user_id, &final_status).await?;
    let updated = find_by_id(&txn, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated employee not found".into()))?;
    txn.commit().await?;
    Ok(updated)
}

/// Demographics + emergency contact (self-service or HR). Does not change org assignment.
pub struct PersonalProfilePatch {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub nationality: Option<String>,
    pub blood_group: Option<String>,
    pub emergency_contact_name: Option<String>,
    pub emergency_contact_phone: Option<String>,
    pub emergency_contact_relation: Option<String>,
}

pub async fn update_personal_profile(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    patch: PersonalProfilePatch,
) -> KabiPayResult<employee::Model> {
    let existing = find_by_id(db, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    let mut am: employee::ActiveModel = existing.into();
    if let Some(v) = patch.first_name {
        let t = v.trim();
        if t.is_empty() {
            return Err(KabiPayError::Validation("firstName cannot be empty".into()));
        }
        am.first_name = Set(t.to_string());
    }
    if let Some(v) = patch.last_name {
        let t = v.trim();
        if t.is_empty() {
            return Err(KabiPayError::Validation("lastName cannot be empty".into()));
        }
        am.last_name = Set(t.to_string());
    }
    if let Some(d) = patch.date_of_birth {
        am.date_of_birth = Set(Some(d));
    }
    if let Some(g) = patch.gender {
        am.gender = Set(Some(g));
    }
    if let Some(n) = patch.nationality {
        am.nationality = Set(Some(n));
    }
    if let Some(bg) = patch.blood_group {
        let t = bg.trim();
        am.blood_group = Set(if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        });
    }
    if let Some(v) = patch.emergency_contact_name {
        am.emergency_contact_name = Set(Some(v));
    }
    if let Some(v) = patch.emergency_contact_phone {
        am.emergency_contact_phone = Set(Some(v));
    }
    if let Some(v) = patch.emergency_contact_relation {
        am.emergency_contact_relation = Set(Some(v));
    }
    am.updated_at = Set(Utc::now());
    am.update(db).await?;
    find_by_id(db, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated employee not found".into()))
}

/// Fields employees may update directly without changing legal identity or organization assignment.
pub struct SelfServiceProfilePatch {
    pub personal_phone: Option<String>,
    pub current_address: Option<String>,
    pub permanent_address: Option<String>,
    pub gender: Option<String>,
    pub nationality: Option<String>,
    pub blood_group: Option<String>,
    pub emergency_contact_name: Option<String>,
    pub emergency_contact_phone: Option<String>,
    pub emergency_contact_relation: Option<String>,
}

fn trimmed_optional(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub async fn update_self_service_profile(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    patch: SelfServiceProfilePatch,
) -> KabiPayResult<employee::Model> {
    let existing = find_by_id(db, tenant_id, employee_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    let mut active: employee::ActiveModel = existing.into();
    if let Some(value) = patch.personal_phone {
        let value = trimmed_optional(value);
        if value.as_ref().is_some_and(|phone| phone.len() > 50) {
            return Err(KabiPayError::Validation(
                "personalPhone must be 50 characters or fewer".into(),
            ));
        }
        active.personal_phone = Set(value);
    }
    if let Some(value) = patch.current_address {
        active.current_address = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.permanent_address {
        active.permanent_address = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.gender {
        active.gender = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.nationality {
        active.nationality = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.blood_group {
        active.blood_group = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.emergency_contact_name {
        active.emergency_contact_name = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.emergency_contact_phone {
        active.emergency_contact_phone = Set(trimmed_optional(value));
    }
    if let Some(value) = patch.emergency_contact_relation {
        active.emergency_contact_relation = Set(trimmed_optional(value));
    }
    active.updated_at = Set(Utc::now());
    active.update(db).await.map_err(KabiPayError::from)
}
