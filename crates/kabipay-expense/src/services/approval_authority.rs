use kabipay_common::{
    client_data_scope::resolve_employee_scope_filter_with_connection,
    context::{ClientClaims, ClientViewerEmployee, ScopeType, PERM_EXPENSE_APPROVE},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0025_workflow::workflow_instance,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, QuerySelect,
};
use sea_orm::sea_query::LockType;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct ExpenseApprovalAuthority {
    pub actor_user_id: Uuid,
    pub actor_employee_id: Option<Uuid>,
    pub scope: ScopeType,
}

impl ExpenseApprovalAuthority {
    pub fn from_claims(claims: &ClientClaims) -> KabiPayResult<Self> {
        if !claims.can_approve_expense() {
            return Err(KabiPayError::Forbidden(
                "expense:approve permission is required".into(),
            ));
        }
        Ok(Self {
            actor_user_id: claims.sub,
            actor_employee_id: claims.employee_id,
            scope: claims.scope_for_permission(PERM_EXPENSE_APPROVE),
        })
    }
}

pub async fn target_is_allowed<C>(
    conn: &C,
    tenant_id: Uuid,
    authority: &ExpenseApprovalAuthority,
    target_employee_id: Uuid,
) -> KabiPayResult<bool>
where
    C: ConnectionTrait + Sync,
{
    if authority.actor_employee_id == Some(target_employee_id) {
        return Ok(false);
    }

    let viewer = match authority.actor_employee_id {
        None => None,
        Some(employee_id) => employee::Entity::find_by_id(employee_id)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(conn)
            .await?
            .map(|row| ClientViewerEmployee {
                employee_id: row.id,
                department_id: row.department_id,
            }),
    };
    let filter = resolve_employee_scope_filter_with_connection(
        conn,
        tenant_id,
        authority.scope,
        viewer,
    )
    .await?;
    Ok(filter.allows_employee(target_employee_id))
}

pub async fn assert_target_allowed<C>(
    conn: &C,
    tenant_id: Uuid,
    authority: &ExpenseApprovalAuthority,
    target_employee_id: Uuid,
) -> KabiPayResult<()>
where
    C: ConnectionTrait + Sync,
{
    if target_is_allowed(conn, tenant_id, authority, target_employee_id).await? {
        Ok(())
    } else {
        Err(KabiPayError::Forbidden(
            "expense approval target is outside your permission scope or is your own request"
                .into(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTransitionSnapshot {
    pub instance_id: Uuid,
    pub current_step_id: Option<Uuid>,
    pub status_in_progress: bool,
}

pub async fn snapshot_workflow_transition(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    instance_id: Uuid,
) -> KabiPayResult<WorkflowTransitionSnapshot> {
    let row = workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Validation("workflow instance not found".into()))?;
    Ok(WorkflowTransitionSnapshot {
        instance_id,
        current_step_id: row.current_step_id,
        status_in_progress: row.status == "IN_PROGRESS",
    })
}

pub async fn lock_matching_workflow_transition(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    expected: WorkflowTransitionSnapshot,
) -> KabiPayResult<workflow_instance::Model> {
    let row = workflow_instance::Entity::find_by_id(expected.instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .lock(LockType::Update)
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::Validation("workflow instance not found".into()))?;

    if row.current_step_id != expected.current_step_id
        || (row.status == "IN_PROGRESS") != expected.status_in_progress
    {
        return Err(KabiPayError::Conflict(
            "approval state changed while this action was being processed; refresh and try again"
                .into(),
        ));
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kabipay_common::context::CLIENT_JWT_ISSUER;
    use std::collections::HashMap;

    fn claims(roles: &[&str], permissions: &[&str]) -> ClientClaims {
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: roles.iter().map(|value| (*value).into()).collect(),
            permissions: permissions.iter().map(|value| (*value).into()).collect(),
            permission_scopes: HashMap::from([(
                PERM_EXPENSE_APPROVE.into(),
                "TEAM".into(),
            )]),
            resource_scopes: HashMap::new(),
        }
    }

    #[test]
    fn role_name_does_not_create_expense_approval_authority() {
        assert!(ExpenseApprovalAuthority::from_claims(&claims(&["HR_ADMIN"], &[])).is_err());
    }

    #[test]
    fn permission_and_action_scope_create_expense_approval_authority() {
        let authority = ExpenseApprovalAuthority::from_claims(&claims(
            &[],
            &[PERM_EXPENSE_APPROVE],
        ))
        .expect("explicit approval permission");
        assert_eq!(authority.scope, ScopeType::Team);
    }
}
