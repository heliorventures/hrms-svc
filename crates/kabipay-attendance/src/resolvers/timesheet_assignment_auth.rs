//! Authorization for per-employee timesheet project assignments.

use async_graphql::{Context, Result};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_context, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::PERM_TIMESHEET_APPROVE,
    subgraph::{require_client_claims, resolve_client_employee_id},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

pub async fn assert_can_read_employee_assignment_target(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    let viewer_id = resolve_client_employee_id(ctx, db, tenant_id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    if viewer_id == target_employee_id {
        return Ok(());
    }

    if claims.can_manage_timesheet_configuration() {
        return employee_exists(db, tenant_id, target_employee_id).await;
    }

    if !claims.can_approve_timesheet_requests() {
        return Err(
            KabiPayError::Forbidden("cannot view project assignments for this employee".into())
                .into_graphql(),
        );
    }

    assert_target_in_timesheet_scope(ctx, db, tenant_id, target_employee_id).await
}

pub async fn assert_can_write_employee_assignment_target(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    let claims = require_client_claims(ctx)?;

    if claims.can_manage_timesheet_configuration() {
        return employee_exists(db, tenant_id, target_employee_id).await;
    }

    if !claims.can_approve_timesheet_requests() {
        return Err(
            KabiPayError::Forbidden(
                "set employee timesheet projects requires timesheet:approve or timesheet:manage"
                    .into(),
            )
            .into_graphql(),
        );
    }

    assert_target_in_timesheet_scope(ctx, db, tenant_id, target_employee_id).await
}

async fn employee_exists(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    let exists = employee::Entity::find_by_id(target_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(|e| KabiPayError::from(e).into_graphql())?;
    if exists.is_none() {
        return Err(KabiPayError::Validation("employee not found".into()).into_graphql());
    }
    Ok(())
}

async fn assert_target_in_timesheet_scope(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    let scope = data_scope_from_context(ctx, PERM_TIMESHEET_APPROVE)?;
    let viewer = resolve_viewer_employee(ctx, db, tenant_id).await?;
    let filt = resolve_employee_scope_filter(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    if filt.allows_employee(target_employee_id) {
        Ok(())
    } else {
        Err(
            KabiPayError::Forbidden("employee is outside your timesheet approval scope".into())
                .into_graphql(),
        )
    }
}
