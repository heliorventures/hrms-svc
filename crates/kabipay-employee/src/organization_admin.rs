//! Typed company administration. Credentials and personal financial data never leave this adapter.
use crate::services::{employee_service as employees, rbac_admin_service as rbac};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{
    AuthorityKind, OrganizationCommand, OrganizationOperation as Op,
};
use kabipay_organization_tenant::{statement, unavailable, VerifiedRuntimeAuthorization};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmployeeCreateInput {
    employee_code: String,
    first_name: String,
    last_name: String,
    date_of_joining: chrono::NaiveDate,
    department_id: Option<Uuid>,
    designation_id: Option<Uuid>,
    employment_type: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct EmployeeUpdateInput {
    first_name: Option<String>,
    last_name: Option<String>,
    department_id: Option<Uuid>,
    designation_id: Option<Uuid>,
    employment_type: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmploymentInput {
    status: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountCreateInput {
    username: String,
    email: Option<String>,
    initial_password: String,
    role_ids: Vec<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountUpdateInput {
    username: Option<String>,
    email: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountAccessInput {
    is_active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountRolesInput {
    role_ids: Vec<Uuid>,
}

fn nonempty(value: &str, field: &str) -> KabiPayResult<()> {
    if value.trim().is_empty() || value.len() > 200 {
        return Err(KabiPayError::Validation(format!(
            "{field} must contain between 1 and 200 characters"
        )));
    }
    Ok(())
}

pub(super) async fn execute(
    db: &DatabaseTransaction,
    p: &VerifiedRuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    authorize_company_admin(
        p.authority_kind.clone(),
        &p.employee_scope,
        &p.capability,
        c.operation,
    )?;
    let tenant = p.tenant_id;
    match c.operation {
        Op::EmployeeLoginProvision => {
            let i: AccountCreateInput = serde_json::from_value(c.payload.clone())?;
            if i.initial_password.chars().count() < 12 || i.initial_password.len() > 256 {
                return Err(KabiPayError::Validation(
                    "initial_password must contain at least 12 characters and at most 256 bytes"
                        .into(),
                ));
            }
            let password_hash = tokio::task::spawn_blocking(move || {
                kabipay_common::password::hash(&i.initial_password)
            })
            .await
            .map_err(|_| KabiPayError::Internal("Password preparation failed".into()))??;
            let e = employees::provision_login_in_transaction(
                db,
                tenant,
                c.resource_id,
                employees::NewLoginAccount {
                    username: i.username,
                    email: i.email,
                    password_hash,
                    role_ids: i.role_ids,
                },
            )
            .await?;
            return Ok(
                serde_json::json!({"employee_id":e.id,"user_id":e.user_id,"has_login":e.user_id.is_some()}),
            );
        }
        Op::EmployeeList => {
            if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
                return Err(unavailable());
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Page {
                after: Option<Uuid>,
                first: u32,
            }
            let i: Page = serde_json::from_value(c.payload.clone())?;
            if i.first == 0 || i.first > 100 || i.after.is_some_and(|id| id.is_nil()) {
                return Err(KabiPayError::Validation(
                    "valid employee cursor and page size 1..100 required".into(),
                ));
            }
            let rows=db.query_all(statement("SELECT e.id,e.employee_code,e.first_name,e.last_name,e.status,v.version FROM employee e JOIN organization_resource_version v ON v.tenant_id=e.tenant_id AND v.resource_type='employee' AND v.resource_id=e.id WHERE e.tenant_id=$1 AND NOT e.is_deleted AND ($2::uuid IS NULL OR e.id>$2) ORDER BY e.id LIMIT $3",vec![tenant.into(),i.after.into(),i64::from(i.first+1).into()])).await?;
            let more = rows.len() > i.first as usize;
            let mut employees = Vec::new();
            for r in rows.into_iter().take(i.first as usize) {
                employees.push(serde_json::json!({"employee_id":r.try_get::<Uuid>("","id")?,"employee_code":r.try_get::<String>("","employee_code")?,"display_name":format!("{} {}",r.try_get::<String>("","first_name")?,r.try_get::<String>("","last_name")?),"status":r.try_get::<String>("","status")?,"version":r.try_get::<i64>("","version")?}));
            }
            return Ok(
                serde_json::json!({"after":employees.last().and_then(|e|e.get("employee_id")),"has_more":more,"employees":employees}),
            );
        }
        Op::EmployeeReferences => {
            let _: kabipay_organization_tenant::EmptyPayload =
                serde_json::from_value(c.payload.clone())?;
            if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
                return Err(unavailable());
            }
            return references(db, tenant).await;
        }
        Op::EmployeeDetail => {
            let _: kabipay_organization_tenant::EmptyPayload =
                serde_json::from_value(c.payload.clone())?;
        }
        Op::EmployeeCreate => {
            let i: EmployeeCreateInput = serde_json::from_value(c.payload.clone())?;
            nonempty(&i.employee_code, "employee_code")?;
            nonempty(&i.first_name, "first_name")?;
            nonempty(&i.last_name, "last_name")?;
            employees::create_with_id(
                db,
                tenant,
                c.resource_id,
                employees::NewEmployee {
                    employee_code: i.employee_code,
                    first_name: i.first_name,
                    last_name: i.last_name,
                    date_of_joining: i.date_of_joining,
                    department_id: i.department_id,
                    designation_id: i.designation_id,
                    reporting_manager_id: None,
                    employment_type: i.employment_type,
                    status: "ACTIVE".into(),
                    user_id: None,
                },
            )
            .await?;
        }
        Op::EmployeeUpdate | Op::EmployeeEmployment => {
            let mut patch = employees::EmployeePatch {
                first_name: None,
                last_name: None,
                department_id: None,
                designation_id: None,
                reporting_manager_id: None,
                employment_type: None,
                status: None,
                user_id: None,
                linked_user_email: None,
            };
            if c.operation == Op::EmployeeEmployment {
                let i: EmploymentInput = serde_json::from_value(c.payload.clone())?;
                let status = kabipay_common::context::canonical_employment_status(&i.status)?;
                patch.status = Some(status.into());
            } else {
                let i: EmployeeUpdateInput = serde_json::from_value(c.payload.clone())?;
                if let Some(v) = &i.first_name {
                    nonempty(v, "first_name")?;
                }
                if let Some(v) = &i.last_name {
                    nonempty(v, "last_name")?;
                }
                patch.first_name = i.first_name;
                patch.last_name = i.last_name;
                patch.department_id = i.department_id;
                patch.designation_id = i.designation_id;
                patch.employment_type = i.employment_type;
            }
            employees::update_in_transaction(
                db,
                tenant,
                c.resource_id,
                patch,
                &p.actor_target_user_ids,
            )
            .await?;
        }
        Op::AccountList => {
            if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
                return Err(unavailable());
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Page {
                after: Option<Uuid>,
                first: u32,
            }
            let i: Page = serde_json::from_value(c.payload.clone())?;
            if i.first == 0 || i.first > 100 {
                return Err(KabiPayError::Validation(
                    "Account page size must be between 1 and 100".into(),
                ));
            }
            let rows=db.query_all(statement("SELECT u.id,u.username,u.email,u.is_active,v.version,(SELECT COALESCE(jsonb_agg(ur.role_id ORDER BY ur.role_id),'[]'::jsonb) FROM user_role ur WHERE ur.user_id=u.id) AS role_ids FROM \"user\" u JOIN organization_resource_version v ON v.tenant_id=u.tenant_id AND v.resource_type='user' AND v.resource_id=u.id WHERE u.tenant_id=$1 AND NOT u.is_deleted AND ($2::uuid IS NULL OR u.id>$2) ORDER BY u.id LIMIT $3",vec![tenant.into(),i.after.into(),i64::from(i.first).into()])).await?;
            let mut accounts = Vec::new();
            for r in rows {
                accounts.push(serde_json::json!({"user_id":r.try_get::<Uuid>("","id")?,"username":r.try_get::<String>("","username")?,"email":r.try_get::<Option<String>>("","email")?,"is_active":r.try_get::<bool>("","is_active")?,"version":r.try_get::<i64>("","version")?,"role_ids":r.try_get::<serde_json::Value>("","role_ids")?}));
            }
            let roles=db.query_all(statement("SELECT id,name FROM role WHERE tenant_id=$1 AND NOT is_deleted ORDER BY name,id LIMIT 500",vec![tenant.into()])).await?.into_iter().map(|r|Ok(serde_json::json!({"role_id":r.try_get::<Uuid>("","id")?,"name":r.try_get::<String>("","name")?}))).collect::<Result<Vec<_>,sea_orm::DbErr>>()?;
            return Ok(
                serde_json::json!({"after":accounts.last().and_then(|a|a.get("user_id")),"has_more":accounts.len()==i.first as usize,"accounts":accounts,"available_roles":roles}),
            );
        }
        Op::AccountCreate => {
            let i: AccountCreateInput = serde_json::from_value(c.payload.clone())?;
            if i.initial_password.chars().count() < 12 || i.initial_password.len() > 256 {
                return Err(KabiPayError::Validation(
                    "initial_password must contain at least 12 characters and at most 256 bytes"
                        .into(),
                ));
            }
            let roles = rbac::validated_active_role_ids(db, tenant, &i.role_ids).await?;
            let password_hash = tokio::task::spawn_blocking(move || {
                kabipay_common::password::hash(&i.initial_password)
            })
            .await
            .map_err(|_| KabiPayError::Internal("Password preparation failed".into()))??;
            employees::insert_login_user_with_id(
                db,
                tenant,
                c.resource_id,
                employees::NewLoginAccount {
                    username: i.username,
                    email: i.email,
                    password_hash,
                    role_ids: i.role_ids,
                },
                roles,
                "ACTIVE",
            )
            .await?;
        }
        Op::AccountRoles => {
            let i: AccountRolesInput = serde_json::from_value(c.payload.clone())?;
            rbac::guard_account_change(
                db,
                tenant,
                c.resource_id,
                p.actor_target_user_ids.contains(&c.resource_id),
                Some(&i.role_ids),
                None,
            )
            .await?;
            rbac::replace_user_roles_in_transaction(db, tenant, c.resource_id, &i.role_ids).await?;
        }
        Op::AccountAccess | Op::AccountUpdate => {
            if c.operation == Op::AccountAccess {
                let i: AccountAccessInput = serde_json::from_value(c.payload.clone())?;
                rbac::set_account_access_in_transaction(
                    db,
                    tenant,
                    c.resource_id,
                    i.is_active,
                    p.actor_target_user_ids.contains(&c.resource_id),
                )
                .await?;
            } else {
                let i: AccountUpdateInput = serde_json::from_value(c.payload.clone())?;
                rbac::update_account_in_transaction(db, tenant, c.resource_id, i.username, i.email)
                    .await?;
            }
        }
        _ => return crate::organization_offboarding::execute(db, p, c).await,
    }
    if matches!(
        c.operation,
        Op::EmployeeDetail | Op::EmployeeCreate | Op::EmployeeUpdate | Op::EmployeeEmployment
    ) {
        let e = employees::find_by_id(db, tenant, c.resource_id)
            .await?
            .ok_or_else(unavailable)?;
        return Ok(
            serde_json::json!({"employee_id":e.id,"employee_code":e.employee_code,"first_name":e.first_name,"last_name":e.last_name,"date_of_joining":e.date_of_joining,"department_id":e.department_id,"designation_id":e.designation_id,"employment_type":e.employment_type,"status":e.status,"has_login":e.user_id.is_some(),"source_references":references(db,tenant).await?}),
        );
    }
    let r=db.query_one(statement("SELECT id,username,email,is_active FROM \"user\" WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
    let roles = db
        .query_all(statement(
            "SELECT role_id FROM user_role WHERE user_id=$1 ORDER BY role_id",
            vec![c.resource_id.into()],
        ))
        .await?
        .into_iter()
        .map(|r| r.try_get::<Uuid>("", "role_id"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        serde_json::json!({"user_id":r.try_get::<Uuid>("","id")?,"username":r.try_get::<String>("","username")?,"email":r.try_get::<Option<String>>("","email")?,"is_active":r.try_get::<bool>("","is_active")?,"role_ids":roles}),
    )
}

async fn references(db: &DatabaseTransaction, tenant: Uuid) -> KabiPayResult<serde_json::Value> {
    // Fixed source queries; no caller-supplied table, filter or tenant selector.
    let mut result = serde_json::json!({"statuses":kabipay_common::context::EMPLOYMENT_STATUSES});
    for (key, query) in [
        ("departments", "SELECT id,name FROM department WHERE tenant_id=$1 AND NOT is_deleted ORDER BY name,id LIMIT 1001"),
        ("designations", "SELECT id,title AS name FROM designation WHERE tenant_id=$1 AND NOT is_deleted ORDER BY title,id LIMIT 1001"),
    ] {
        let rows=db.query_all(statement(query,vec![tenant.into()])).await?;
        if rows.len()>1000 { return Err(KabiPayError::Validation("source reference list exceeds supported size".into())); }
        result[key]=serde_json::Value::Array(rows.into_iter().map(|r|Ok(serde_json::json!({"id":r.try_get::<Uuid>("","id")?,"name":r.try_get::<String>("","name")?}))).collect::<Result<Vec<_>,sea_orm::DbErr>>()?);
    }
    let rows=db.query_all(statement("SELECT DISTINCT employment_type FROM employee WHERE tenant_id=$1 AND NOT is_deleted AND employment_type IS NOT NULL ORDER BY employment_type LIMIT 1001",vec![tenant.into()])).await?;
    if rows.len() > 1000 {
        return Err(KabiPayError::Validation(
            "source employment type list exceeds supported size".into(),
        ));
    }
    result["employment_types"] = serde_json::to_value(
        rows.into_iter()
            .map(|r| r.try_get::<String>("", "employment_type"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    Ok(result)
}

fn authorize_company_admin(
    kind: AuthorityKind,
    scope: &str,
    capability: &str,
    operation: Op,
) -> KabiPayResult<()> {
    if kind != AuthorityKind::OrganizationGrant
        || scope != "ALL"
        || capability != operation.capability()
    {
        return Err(unavailable());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn employee_commands_cannot_relink_users_or_change_protected_fields() {
        for key in [
            "user_id",
            "status",
            "linked_user_email",
            "password_hash",
            "uan_number",
            "reporting_manager_id",
        ] {
            assert!(serde_json::from_value::<EmployeeUpdateInput>(
                serde_json::json!({key:"value"})
            )
            .is_err());
        }
    }
    #[test]
    fn account_access_requires_boolean_and_does_not_accept_credentials() {
        assert!(serde_json::from_value::<AccountAccessInput>(
            serde_json::json!({"is_active":true,"password_hash":"secret"})
        )
        .is_err());
        assert!(serde_json::from_value::<AccountAccessInput>(
            serde_json::json!({"is_active":"true"})
        )
        .is_err());
    }
    #[test]
    fn blank_identity_fields_are_rejected() {
        assert!(nonempty("  ", "first_name").is_err());
    }
    #[test]
    fn administration_requires_explicit_company_grant_for_exact_domain() {
        for operation in [
            Op::EmployeeUpdate,
            Op::AccountRoles,
            Op::WorkflowSave,
            Op::SeparationReject,
        ] {
            assert!(authorize_company_admin(
                AuthorityKind::OrganizationGrant,
                "ALL",
                operation.capability(),
                operation
            )
            .is_ok());
            for scope in ["SELF", "TEAM", "READ", "all"] {
                assert!(authorize_company_admin(
                    AuthorityKind::OrganizationGrant,
                    scope,
                    operation.capability(),
                    operation
                )
                .is_err());
            }
            assert!(authorize_company_admin(
                AuthorityKind::OrganizationGrant,
                "ALL",
                "organization:manage",
                operation
            )
            .is_err());
            assert!(authorize_company_admin(
                AuthorityKind::ManagementAssignment,
                "ALL",
                operation.capability(),
                operation
            )
            .is_err());
        }
    }
}
