//! Permission- and scope-based workflow approval eligibility.
//!
//! Roles assign permissions during authentication. Runtime workflow decisions consume only the
//! effective permission and scope carried by the request, plus reporting-manager relationships
//! configured by the workflow step.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0025_workflow::workflow_step;

use crate::client_data_scope::{
    resolve_employee_scope_filter_with_connection, EmployeeScopeFilter,
};
use crate::context::{ClientViewerEmployee, ScopeType};
use crate::error::{KabiPayError, KabiPayResult};

#[derive(Clone, Debug)]
pub struct WorkflowApprovalAuthority {
    pub actor_user_id: Uuid,
    pub actor_employee: Option<ClientViewerEmployee>,
    pub scope: ScopeType,
    pub permission: &'static str,
}

fn assert_not_self_approval(
    authority: &WorkflowApprovalAuthority,
    subject_employee_id: Uuid,
) -> KabiPayResult<()> {
    if authority
        .actor_employee
        .is_some_and(|actor| actor.employee_id == subject_employee_id)
    {
        return Err(KabiPayError::Forbidden(
            "you cannot approve or reject your own request".into(),
        ));
    }
    Ok(())
}

fn approval_scope_allows(filter: &EmployeeScopeFilter, subject_employee_id: Uuid) -> bool {
    filter.allows_employee(subject_employee_id)
}

fn normalize_approver_type(raw: &Option<String>) -> String {
    raw.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("REPORTING_MANAGER")
        .to_ascii_uppercase()
}

fn step_requires_authority_permission(
    step: &workflow_step::Model,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<()> {
    let configured = step
        .approver_permission
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            KabiPayError::Validation(
                "workflow step is missing its required approver permission; migrate or update the workflow definition"
                    .into(),
            )
        })?;
    if configured.eq_ignore_ascii_case(authority.permission) {
        Ok(())
    } else {
        Err(KabiPayError::Forbidden(
            "the current session does not contain the permission required by this workflow step"
                .into(),
        ))
    }
}

async fn load_subject_employee(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    subject_employee_id: Uuid,
) -> KabiPayResult<employee::Model> {
    employee::Entity::find_by_id(subject_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(conn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: subject_employee_id.to_string(),
        })
}

pub async fn assert_subject_in_approval_scope(
    conn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<()> {
    assert_not_self_approval(authority, subject_employee_id)?;
    let subject = load_subject_employee(conn, tenant_id, subject_employee_id).await?;
    let filter = resolve_employee_scope_filter_with_connection(
        conn,
        tenant_id,
        authority.scope,
        authority.actor_employee,
    )
    .await?;
    if !approval_scope_allows(&filter, subject.id) {
        return Err(KabiPayError::Forbidden(
            "the request is outside your approval scope".into(),
        ));
    }
    Ok(())
}

async fn assert_is_reporting_manager_user(
    conn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    authority: &WorkflowApprovalAuthority,
    subject_employee_id: Uuid,
) -> KabiPayResult<()> {
    let subject = load_subject_employee(conn, tenant_id, subject_employee_id).await?;
    let manager_employee_id = subject.reporting_manager_id.ok_or_else(|| {
        KabiPayError::Validation(
            "employee has no reporting manager; assign a manager before this workflow step can be completed"
                .into(),
        )
    })?;
    let manager = employee::Entity::find_by_id(manager_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(conn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("reporting manager employee record not found".into())
        })?;
    match manager.user_id {
        Some(user_id) if user_id == authority.actor_user_id => Ok(()),
        Some(_) => Err(KabiPayError::Forbidden(
            "only the employee's reporting manager can act at this workflow step".into(),
        )),
        None => Err(KabiPayError::Validation(
            "reporting manager has no linked login account".into(),
        )),
    }
}

/// Ensures the request is in the exact permission scope and that the actor matches the configured
/// workflow relationship/permission rule. Legacy role-only steps fail closed until migrated.
pub async fn assert_workflow_step_actor(
    conn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    step: &workflow_step::Model,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<()> {
    assert_subject_in_approval_scope(conn, tenant_id, subject_employee_id, authority).await?;
    match normalize_approver_type(&step.approver_type).as_str() {
        "REPORTING_MANAGER" | "MANAGER" | "LINE_MANAGER" => {
            assert_is_reporting_manager_user(conn, tenant_id, authority, subject_employee_id).await
        }
        "PERMISSION" => step_requires_authority_permission(step, authority),
        "REPORTING_MANAGER_OR_PERMISSION" | "MANAGER_OR_PERMISSION" => {
            if assert_is_reporting_manager_user(conn, tenant_id, authority, subject_employee_id)
                .await
                .is_ok()
            {
                return Ok(());
            }
            step_requires_authority_permission(step, authority)
        }
        "ROLE" | "REPORTING_MANAGER_OR_ROLE" | "MANAGER_OR_ROLE" => {
            Err(KabiPayError::Validation(
                "role-based workflow steps are no longer valid runtime authority; migrate the step to a required permission"
                    .into(),
            ))
        }
        other => Err(KabiPayError::Validation(format!(
            "unsupported workflow_step.approver_type: {other}"
        ))),
    }
}

/// Compatibility entry point for existing timesheet callers. Runtime semantics are identical:
/// exact permission/scope plus the configured manager relationship.
pub async fn assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
    conn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    step: &workflow_step::Model,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<()> {
    assert_workflow_step_actor(conn, tenant_id, subject_employee_id, step, authority).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(permission: &'static str) -> WorkflowApprovalAuthority {
        WorkflowApprovalAuthority {
            actor_user_id: Uuid::nil(),
            actor_employee: None,
            scope: ScopeType::All,
            permission,
        }
    }

    fn step(kind: &str, permission: Option<&str>) -> workflow_step::Model {
        workflow_step::Model {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            workflow_id: Uuid::nil(),
            sequence_order: 1,
            step_name: "Approval".into(),
            approver_type: Some(kind.into()),
            approver_role_id: None,
            approver_permission: permission.map(str::to_owned),
            can_skip: false,
            sla_hours: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn exact_step_permission_is_required() {
        let auth = authority("leave:approve");
        assert!(step_requires_authority_permission(
            &step("PERMISSION", Some("leave:approve")),
            &auth
        )
        .is_ok());
        assert!(step_requires_authority_permission(
            &step("PERMISSION", Some("expense:approve")),
            &auth
        )
        .is_err());
        assert!(step_requires_authority_permission(&step("PERMISSION", None), &auth).is_err());
    }

    #[test]
    fn self_approval_is_denied_independently_of_the_resolved_scope() {
        let actor_employee_id = Uuid::new_v4();
        let auth = WorkflowApprovalAuthority {
            actor_user_id: Uuid::new_v4(),
            actor_employee: Some(ClientViewerEmployee {
                employee_id: actor_employee_id,
                department_id: None,
            }),
            scope: ScopeType::All,
            permission: "leave:approve",
        };

        let error = assert_not_self_approval(&auth, actor_employee_id)
            .expect_err("ALL scope must not permit self-approval");

        assert!(matches!(error, KabiPayError::Forbidden(_)));
    }

    #[test]
    fn approval_scope_consumes_resolved_recursive_employee_ids() {
        let actor_employee_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let filter = crate::client_data_scope::EmployeeScopeFilter::EmployeeIds(vec![
            actor_employee_id,
            descendant_id,
        ]);

        assert!(approval_scope_allows(&filter, descendant_id));
        assert!(!approval_scope_allows(&filter, outside_id));
    }
}
