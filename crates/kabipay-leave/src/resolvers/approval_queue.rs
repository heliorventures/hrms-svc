//! Complete scoped queue: keyset scan keeps memory bounded while exact workflow
//! authorization is evaluated before counting and selecting the requested page.
use async_graphql::{Context, Result, SimpleObject};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::client_data_scope::{
    resolve_employee_scope_filter, resolve_viewer_employee, EmployeeScopeFilter,
};
use kabipay_common::context::ScopeType;
use kabipay_common::subgraph::tenant_db;
use kabipay_common::KabiPayError;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0011_leave::leave_request;
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::collections::HashMap;
use uuid::Uuid;
use super::types::LeaveRequestDto;

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
        query = query.filter(
            Condition::any()
                .add(leave_request::Column::AppliedAt.lt(applied_at))
                .add(Condition::all()
                    .add(leave_request::Column::AppliedAt.eq(applied_at))
                    .add(leave_request::Column::Id.lt(id))),
        );
    }
    query
        .order_by_desc(leave_request::Column::AppliedAt)
        .order_by_desc(leave_request::Column::Id)
        .limit(200)
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
    let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
    let filter = resolve_employee_scope_filter(&db, tenant_id, scope, viewer).await.map_err(KabiPayError::into_graphql)?;
    let mut page = QueuePage::new(limit, offset);
    let mut cursor = None;
    loop {
        let candidates = candidate_query(tenant_id, &filter, from, to, cursor).all(&db).await
            .map_err(|error| KabiPayError::from(error).into_graphql())?;
        if candidates.is_empty() {
            break;
        }
        cursor = candidates.last().map(|row| (row.applied_at, row.id));
        let last_batch = candidates.len() < 200;
        for row in candidates {
            let row_status = row.status.clone();
            let dto = LeaveRequestDto::from(row);
            // This is the same cached resolver used by viewerMayApprove and
            // pendingApprovalStepId; it does not infer authority from read scope.
            let actionable = row_status == "PENDING" && dto.actionable_approval_step_id(ctx).await?.is_some();
            page.consider(dto, &row_status, actionable, status, needs_my_action);
        }
        if last_batch {
            break;
        }
    }

    // Enrich only the selected page, retaining the authorization cache.
    if !page.rows.is_empty() {
        let employee_ids: Vec<Uuid> = page.rows.iter().map(|row| super::query::parse_uuid(&row.employee_id, "employeeId"))
            .collect::<Result<_>>()?;
        let employees: HashMap<_, _> = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .filter(employee::Column::Id.is_in(employee_ids))
            .all(&db).await.map_err(|error| KabiPayError::from(error).into_graphql())?
            .into_iter().map(|row| (row.id.to_string(), row)).collect();
        let file_ids: Vec<Uuid> = page.rows.iter().filter_map(|row| row.supporting_document_file_storage_id.as_ref())
            .map(|id| Uuid::parse_str(id.as_str())).collect::<std::result::Result<_, _>>()
            .map_err(|error| KabiPayError::Validation(error.to_string()).into_graphql())?;
        let files: HashMap<_, _> = if file_ids.is_empty() { HashMap::new() } else {
            file_storage::Entity::find().filter(file_storage::Column::TenantId.eq(tenant_id))
                .filter(file_storage::Column::Id.is_in(file_ids)).all(&db).await
                .map_err(|error| KabiPayError::from(error).into_graphql())?
                .into_iter().map(|row| (row.id.to_string(), row)).collect()
        };
        page.rows = page.rows.into_iter().map(|dto| {
            let person = employees.get(dto.employee_id.as_str());
            let file = dto.supporting_document_file_storage_id.as_ref().and_then(|id| files.get(id.as_str()));
            let dto = dto.with_supporting_document_file(file);
            match person {
                Some(person) => dto.with_employee_label(format!("{} {}", person.first_name, person.last_name).trim().to_owned(), person.employee_code.clone()),
                None => dto,
            }
        }).collect();
    }
    Ok(LeaveApprovalQueue {
        rows: page.rows,
        total_count: page.total_count,
        pending_count: page.pending_count,
        actionable_count: page.actionable_count,
    })
}

#[cfg(test)]
#[path = "approval_queue_tests.rs"]
mod tests;
