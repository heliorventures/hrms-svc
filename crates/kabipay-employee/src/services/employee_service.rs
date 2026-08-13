//! Employee queries and write operations on a tenant-scoped connection.
//!
//! Every query applies both the `tenant_id` filter (Gap A — defence in depth even with
//! schema isolation) and the `is_deleted = false` filter (Gap B — soft-delete policy).

use chrono::{NaiveDate, Utc};
use kabipay_common::client_data_scope::employee_model_in_scope;
use kabipay_common::context::ClientViewerEmployee;
use kabipay_common::context::ScopeType;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{role, user, user_role, user_session};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::entities::d0007_employee_core::employee;

/// Keep an already linked auth user enabled only for active employee statuses.
fn employee_login_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "ACTIVE" | "PROBATION"
    )
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
    let should_be_active = employee_login_is_active(employee_status);
    let Some(found) = user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .one(db)
        .await?
    else {
        return Ok(());
    };
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

/// Every employee in the reporting subtree under `root_employee_id` (including the root).
/// Assumes an acyclic manager chain; terminates when no new direct reports appear.
async fn collect_team_subtree_employee_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    root_employee_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(root_employee_id);
    let mut frontier = vec![root_employee_id];

    loop {
        if frontier.is_empty() {
            break;
        }

        let children = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .filter(employee::Column::ReportingManagerId.is_in(frontier.clone()))
            .all(db)
            .await
            .map_err(KabiPayError::from)?;

        frontier.clear();
        for m in children {
            if seen.insert(m.id) {
                frontier.push(m.id);
            }
        }
    }

    Ok(seen.into_iter().collect())
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

/// Whether a fetched employee row is visible under `scope` (used for `employee(id:)` / IDOR checks).
pub fn is_employee_in_scope(
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
    target: &employee::Model,
) -> bool {
    employee_model_in_scope(scope, viewer, target)
}

/// Resolve all employee IDs visible to a caller for cross-record approval queues.
/// `None` means unrestricted tenant scope; `Some([])` means no visible employees.
pub async fn employee_ids_in_scope(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<Option<Vec<Uuid>>> {
    let Some(viewer) = viewer else {
        return Ok(match scope {
            ScopeType::All => None,
            _ => Some(Vec::new()),
        });
    };
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false));
    query = match scope {
        ScopeType::All => return Ok(None),
        ScopeType::Self_ => query.filter(employee::Column::Id.eq(viewer.employee_id)),
        ScopeType::Team => query.filter(
            Condition::any()
                .add(employee::Column::Id.eq(viewer.employee_id))
                .add(employee::Column::ReportingManagerId.eq(viewer.employee_id)),
        ),
        ScopeType::Department => match viewer.department_id {
            Some(department_id) => query.filter(
                Condition::any()
                    .add(employee::Column::Id.eq(viewer.employee_id))
                    .add(employee::Column::DepartmentId.eq(department_id)),
            ),
            None => query.filter(employee::Column::Id.eq(viewer.employee_id)),
        },
    };
    Ok(Some(query.all(db).await?.into_iter().map(|row| row.id).collect()))
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
    let mut q = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false));

    match scope {
        ScopeType::All => {}
        ScopeType::Self_ => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = q.filter(employee::Column::Id.eq(v.employee_id));
        }
        ScopeType::Team => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = q.filter(
                Condition::any()
                    .add(employee::Column::Id.eq(v.employee_id))
                    .add(employee::Column::ReportingManagerId.eq(v.employee_id)),
            );
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = if let Some(d) = v.department_id {
                q.filter(
                    Condition::any()
                        .add(employee::Column::Id.eq(v.employee_id))
                        .add(employee::Column::DepartmentId.eq(Some(d))),
                )
            } else {
                q.filter(employee::Column::Id.eq(v.employee_id))
            };
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
    let mut q = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false));

    match scope {
        ScopeType::All => {}
        ScopeType::Self_ => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = q.filter(employee::Column::Id.eq(v.employee_id));
        }
        ScopeType::Team => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            let ids = collect_team_subtree_employee_ids(db, tenant_id, v.employee_id).await?;
            if ids.is_empty() {
                return Ok(vec![]);
            }
            q = q.filter(employee::Column::Id.is_in(ids));
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = if let Some(d) = v.department_id {
                q.filter(
                    Condition::any()
                        .add(employee::Column::Id.eq(v.employee_id))
                        .add(employee::Column::DepartmentId.eq(Some(d))),
                )
            } else {
                q.filter(employee::Column::Id.eq(v.employee_id))
            };
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

async fn ensure_role_ids_in_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    role_ids: &[Uuid],
) -> KabiPayResult<Vec<Uuid>> {
    let unique: Vec<Uuid> = role_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for role_id in &unique {
        let role_exists = role::Entity::find_by_id(*role_id)
            .filter(role::Column::TenantId.eq(tenant_id))
            .filter(role::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .is_some();
        if !role_exists {
            return Err(KabiPayError::NotFound {
                entity: "role",
                id: role_id.to_string(),
            });
        }
    }
    Ok(unique)
}

async fn assign_roles<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Uuid,
    role_ids: &[Uuid],
) -> KabiPayResult<()> {
    let roles = ensure_role_ids_in_tenant(db, tenant_id, role_ids).await?;
    let now = Utc::now();
    for role_id in roles {
        user_role::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(role_id),
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
    employee_status: &str,
) -> KabiPayResult<Uuid> {
    let username = normalize_username(&account.username)?;
    let email = normalize_email(account.email);
    let password_hash = account.password_hash;
    let role_ids = account.role_ids;
    if user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::Username.eq(&username))
        .one(db)
        .await?
        .is_some()
    {
        return Err(KabiPayError::Conflict(
            "username is already in use in this tenant".into(),
        ));
    }
    if let Some(ref email_value) = email {
        if user::Entity::find()
            .filter(user::Column::TenantId.eq(tenant_id))
            .filter(user::Column::Email.eq(email_value))
            .one(db)
            .await?
            .is_some()
        {
            return Err(KabiPayError::Conflict(
                "email is already in use in this tenant".into(),
            ));
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
        is_active: Set(employee_login_is_active(employee_status)),
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
    .await?;
    assign_roles(db, tenant_id, id, &role_ids).await?;
    Ok(id)
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    data: NewEmployee,
) -> KabiPayResult<employee::Model> {
    if employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::EmployeeCode.eq(&data.employee_code))
        .one(db)
        .await?
        .is_some()
    {
        return Err(KabiPayError::Conflict(
            "employee code is already in use in this tenant".into(),
        ));
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
    am.insert(db).await?;
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
    let txn = db.begin().await?;
    let user_id = insert_login_user(&txn, tenant_id, account, &data.status).await?;
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
    let txn = db.begin().await?;
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
    let user_id = insert_login_user(&txn, tenant_id, account, &existing.status).await?;
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
    patch: EmployeePatch,
) -> KabiPayResult<employee::Model> {
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
    am.user_id = Set(final_user_id);
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;
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
