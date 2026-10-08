//! Complete scoped queue: keyset scan keeps memory bounded while exact workflow
//! authorization is evaluated before counting and selecting the requested page.
use async_graphql::{Context, Result, SimpleObject};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::client_data_scope::{
    resolve_employee_scope_filter_with_connection, resolve_viewer_employee_with_connection,
    EmployeeScopeFilter,
};
use kabipay_common::context::{is_active_employment_status, ScopeType, PERM_LEAVE_APPROVE};
use kabipay_common::workflow_approval::{
    batch_workflow_step_actor_allows, WorkflowApprovalAuthority, WorkflowApprovalScope,
};
use kabipay_db_entities::tenant::d0025_workflow::{workflow, workflow_instance, workflow_step};
use kabipay_common::subgraph::tenant_db;
use kabipay_common::KabiPayError;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0011_leave::leave_request;
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{
    AccessMode, ColumnTrait, ConnectionTrait, EntityTrait, IsolationLevel, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use sea_orm::sea_query::Expr;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use super::types::LeaveRequestDto;

// Independent of the public page limit: bounds authority inputs while amortizing round trips.
const CANDIDATE_BATCH_SIZE: u64 = 1_000;

#[derive(SimpleObject)]
pub struct LeaveApprovalQueue {
    pub rows: Vec<LeaveRequestDto>,
    pub total_count: u64,
    pub pending_count: u64,
    pub actionable_count: u64,
}

struct QueuePage<T> {
    rows: Vec<T>,
    limit: u64,
    offset: u64,
    total_count: u64,
    pending_count: u64,
    actionable_count: u64,
}

impl<T> QueuePage<T> {
    fn new(limit: u64, offset: u64) -> Self {
        Self {
            rows: Vec::new(),
            limit,
            offset,
            total_count: 0,
            pending_count: 0,
            actionable_count: 0,
        }
    }

    fn consider(
        &mut self,
        row: T,
        status: &str,
        actionable: bool,
        selected_status: Option<&str>,
        needs_my_action: bool,
    ) {
        self.pending_count += u64::from(status == "PENDING");
        self.actionable_count += u64::from(actionable);
        if selected_status.is_some_and(|selected| selected != status) || (needs_my_action && !actionable) {
            return;
        }
        if self.total_count >= self.offset && (self.rows.len() as u64) < self.limit {
            self.rows.push(row);
        }
        self.total_count += 1;
    }
}

fn validate_filters(
    limit: u64,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    status: Option<&str>,
) -> Result<()> {
    let error = if !(1..=200).contains(&limit) {
        Some("limit must be between 1 and 200")
    } else if from.zip(to).is_some_and(|(from, to)| from > to) {
        Some("fromDate must be on or before toDate")
    } else if status.is_some_and(|value| !matches!(value, "PENDING" | "APPROVED" | "REJECTED" | "CANCELLED")) {
        Some("status must be PENDING, APPROVED, REJECTED, or CANCELLED")
    } else {
        None
    };
    match error {
        Some(message) => Err(KabiPayError::Validation(message.into()).into_graphql()),
        None => Ok(()),
    }
}

fn candidate_query(
    tenant_id: Uuid,
    scope: &EmployeeScopeFilter,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> sea_orm::Select<leave_request::Entity> {
    let mut query = leave_request::Entity::find()
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false));
    if let Some(from) = from {
        query = query.filter(leave_request::Column::ToDate.gte(from));
    }
    if let Some(to) = to {
        query = query.filter(leave_request::Column::FromDate.lte(to));
    }
    match scope {
        EmployeeScopeFilter::Unrestricted => {},
        EmployeeScopeFilter::Empty => {
            query = query.filter(leave_request::Column::EmployeeId.is_in(Vec::<Uuid>::new()));
        }
        EmployeeScopeFilter::EmployeeIds(ids) => {
            query = query.filter(leave_request::Column::EmployeeId.is_in(ids.clone()));
        }
    }
    if let Some((applied_at, id)) = cursor {
        // A row comparison lets PostgreSQL seek directly into the composite index.
        // An equivalent OR expression can re-scan all prior rows on every batch.
        query = query.filter(
            Expr::tuple([
                Expr::col((leave_request::Entity, leave_request::Column::AppliedAt)).into(),
                Expr::col((leave_request::Entity, leave_request::Column::Id)).into(),
            ])
            .lt(Expr::tuple([Expr::value(applied_at), Expr::value(id)])),
        );
    }
    query
        .order_by_desc(leave_request::Column::AppliedAt)
        .order_by_desc(leave_request::Column::Id)
        .limit(CANDIDATE_BATCH_SIZE)
}

// Each batch loads bounded sets of primary-key records; authority rules stay in common Rust.
async fn batch_actionable_steps(
    db: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    rows: &[leave_request::Model],
    authority: Option<&WorkflowApprovalAuthority>,
    filter: &WorkflowApprovalScope,
) -> kabipay_common::KabiPayResult<HashMap<Uuid, Uuid>> {
    let Some(authority) = authority else {
        return Ok(HashMap::new());
    };
    let Some(actor) = authority.actor_employee else {
        return Ok(HashMap::new());
    };
    let pending: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.status == "PENDING"
                && row.employee_id != actor.employee_id
                && row.workflow_instance_id.is_some()
        })
        .collect();
    if pending.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: HashSet<_> = pending
        .iter()
        .map(|row| row.employee_id)
        .chain(std::iter::once(actor.employee_id))
        .collect();
    let employees: HashMap<_, _> = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    if !employees.get(&actor.employee_id).is_some_and(|row| {
        row.user_id == Some(authority.actor_user_id) && is_active_employment_status(&row.status)
    }) {
        return Ok(HashMap::new());
    }
    let manager_ids: HashSet<_> = employees
        .values()
        .filter_map(|row| row.reporting_manager_id)
        .collect();
    let managers: HashMap<_, _> = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(manager_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let instance_ids: HashSet<_> = pending
        .iter()
        .filter_map(|row| row.workflow_instance_id)
        .collect();
    let instances: HashMap<_, _> = workflow_instance::Entity::find()
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq("LEAVE_REQUEST"))
        .filter(workflow_instance::Column::Status.eq("IN_PROGRESS"))
        .filter(workflow_instance::Column::Id.is_in(instance_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let workflow_ids: HashSet<_> = instances.values().map(|row| row.workflow_id).collect();
    let workflows: std::collections::HashSet<Uuid> = workflow::Entity::find()
        .select_only()
        .column(workflow::Column::Id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq("LEAVE_REQUEST"))
        .filter(workflow::Column::Id.is_in(workflow_ids))
        .into_tuple()
        .all(db)
        .await?
        .into_iter()
        .collect();
    let step_ids: HashSet<_> = instances.values().filter_map(|row| row.current_step_id).collect();
    let steps: HashMap<_, _> = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::Id.is_in(step_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let mut result = HashMap::new();
    for row in pending {
        let Some(subject) = employees
            .get(&row.employee_id)
            .filter(|row| is_active_employment_status(&row.status))
        else {
            continue;
        };
        let Some(instance) = row.workflow_instance_id.and_then(|id| instances.get(&id)) else {
            continue;
        };
        if instance.entity_id != row.id || !workflows.contains(&instance.workflow_id) {
            continue;
        }
        let Some(step) = instance
            .current_step_id
            .and_then(|id| steps.get(&id))
            .filter(|step| step.workflow_id == instance.workflow_id)
        else {
            continue;
        };
        let manager = subject.reporting_manager_id.and_then(|id| managers.get(&id));
        if batch_workflow_step_actor_allows(subject, manager, filter, step, authority) {
            result.insert(row.id, step.id);
        }
    }
    Ok(result)
}

pub(super) async fn load(
    ctx: &Context<'_>,
    tenant_id: Uuid,
    scope: ScopeType,
    limit: u64,
    offset: u64,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    status: Option<&str>,
    needs_my_action: bool,
) -> Result<LeaveApprovalQueue> {
    validate_filters(limit, from, to, status)?;
    let db = tenant_db(ctx, tenant_id).await?;
    load_from_db(
        ctx, db, tenant_id, scope, limit, offset, from, to, status, needs_my_action,
    )
    .await
}

async fn load_from_db(
    ctx: &Context<'_>,
    db: sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    limit: u64,
    offset: u64,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    status: Option<&str>,
    needs_my_action: bool,
) -> Result<LeaveApprovalQueue> {
    let db = db
        .begin_with_config(Some(IsolationLevel::RepeatableRead), Some(AccessMode::ReadOnly))
        .await
        .map_err(|error| KabiPayError::from(error).into_graphql())?;
    let viewer = resolve_viewer_employee_with_connection(ctx, &db, tenant_id).await?;
    let filter = resolve_employee_scope_filter_with_connection(&db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let claims = kabipay_common::subgraph::require_client_claims(ctx)?;
    let authority = super::types::leave_approval_scope_from_claims(claims).map(|scope| {
        WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: viewer,
            scope,
            permission: PERM_LEAVE_APPROVE,
        }
    });
    let approval_filter = if let Some(authority) = &authority {
        resolve_employee_scope_filter_with_connection(&db, tenant_id, authority.scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?
    } else {
        EmployeeScopeFilter::Empty
    };
    let approval_filter = WorkflowApprovalScope::from(&approval_filter);
    let mut page = QueuePage::new(limit, offset);
    let mut cursor = None;
    loop {
        let candidates = candidate_query(tenant_id, &filter, from, to, cursor).all(&db).await
            .map_err(|error| KabiPayError::from(error).into_graphql())?;
        if candidates.is_empty() {
            break;
        }
        cursor = candidates.last().map(|row| (row.applied_at, row.id));
        let last_batch = (candidates.len() as u64) < CANDIDATE_BATCH_SIZE;
        let actionable = batch_actionable_steps(
            &db, tenant_id, &candidates, authority.as_ref(), &approval_filter,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        for row in candidates {
            let row_status = row.status.clone();
            let step_id = actionable.get(&row.id).copied();
            page.consider((row, step_id), &row_status, step_id.is_some(), status, needs_my_action);
        }
        if last_batch {
            break;
        }
    }

    // Allocate DTO strings and authorization cells only for selected rows.
    let mut rows: Vec<_> = page.rows
        .into_iter()
        .map(|(row, step_id)| LeaveRequestDto::from(row).with_approval_snapshot(step_id))
        .collect();

    // Enrich only the selected page, retaining the authorization cache.
    if !rows.is_empty() {
        let employee_ids: HashSet<Uuid> = rows.iter().map(|row| super::query::parse_uuid(&row.employee_id, "employeeId"))
            .collect::<Result<_>>()?;
        let employees: HashMap<_, _> = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .filter(employee::Column::Id.is_in(employee_ids))
            .all(&db).await.map_err(|error| KabiPayError::from(error).into_graphql())?
            .into_iter().map(|row| (row.id.to_string(), row)).collect();
        let file_ids: HashSet<Uuid> = rows.iter().filter_map(|row| row.supporting_document_file_storage_id.as_ref())
            .map(|id| Uuid::parse_str(id.as_str())).collect::<std::result::Result<_, _>>()
            .map_err(|error| KabiPayError::Validation(error.to_string()).into_graphql())?;
        let files: HashMap<_, _> = if file_ids.is_empty() { HashMap::new() } else {
            file_storage::Entity::find().filter(file_storage::Column::TenantId.eq(tenant_id))
                .filter(file_storage::Column::Id.is_in(file_ids)).all(&db).await
                .map_err(|error| KabiPayError::from(error).into_graphql())?
                .into_iter().map(|row| (row.id.to_string(), row)).collect()
        };
        rows = rows.into_iter().map(|dto| {
            let person = employees.get(dto.employee_id.as_str());
            let file = dto.supporting_document_file_storage_id.as_ref().and_then(|id| files.get(id.as_str()));
            let dto = dto.with_supporting_document_file(file);
            match person {
                Some(person) => dto.with_employee_label(format!("{} {}", person.first_name, person.last_name).trim().to_owned(), person.employee_code.clone()),
                None => dto,
            }
        }).collect();
    }
    let stage_ids = rows.iter().filter(|dto| dto.status.trim().eq_ignore_ascii_case("PENDING"))
        .filter_map(|dto| dto.workflow_instance_id.as_ref()).map(|id| Uuid::parse_str(id.as_str()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| KabiPayError::Validation(error.to_string()).into_graphql())?;
    let stages = kabipay_common::workflow_current_step::pending_step_titles_batch(&db, tenant_id, &stage_ids)
        .await.map_err(KabiPayError::into_graphql)?;
    rows = rows.into_iter().map(|dto| {
        let stage = if dto.status.trim().eq_ignore_ascii_case("PENDING") {
            dto.workflow_instance_id.as_ref().and_then(|id| Uuid::parse_str(id.as_str()).ok())
                .and_then(|id| stages.get(&id)).cloned()
        } else { None };
        dto.with_pending_stage_snapshot(stage)
    }).collect();
    db.commit().await.map_err(|error| KabiPayError::from(error).into_graphql())?;
    Ok(LeaveApprovalQueue {
        rows,
        total_count: page.total_count,
        pending_count: page.pending_count,
        actionable_count: page.actionable_count,
    })
}

#[cfg(test)]
#[path = "approval_queue_tests.rs"]
mod tests;
