//! Root query resolvers for kabipay-employee.

use async_graphql::{Context, Object, Result, ID};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use kabipay_common::{
    client_data_scope::{
        resolve_employee_scope_filter, resolve_employee_scope_filter_with_connection,
        EmployeeScopeFilter,
    },
    context::{
        ScopeType, PERM_EMPLOYEE_READ, PERM_EXPENSE_MANAGE, PERM_ONBOARDING_MANAGE,
        PERM_ONBOARDING_SELF, PERM_PAYROLL_READ,
    },
    subgraph::{
        require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db,
    },
    KabiPayError,
};
use serde::{Deserialize, Serialize};
use std::future::Future;
use uuid::Uuid;

use crate::resolvers::types::{
    ClearanceChecklistItemDto, CompanyDocumentDto, DepartmentDto, DesignationDto, DocumentTypeDto,
    EmployeeAadhaarRecordDto, EmployeeBankAccountDto, EmployeeDirectoryEntryDto,
    EmployeeDirectoryPageDto, EmployeeDocumentAttachmentDto, EmployeeDocumentDto, EmployeeDto,
    EmployeeEducationDto, EmployeeEvidenceReviewQueueItemDto, EmployeeIdentityProfileDto,
    EmployeePanRecordDto, EmployeeProfileAccessDto, EmployeeProfileChangeRequestDto,
    EmployeeProfileChangeReviewDetailDto, EmployeeProfileReviewQueueItemDto,
    EmployeeWorkExperienceDto, EmploymentHistoryRecordDto, FnfSettlementDto,
    OnboardingChecklistItemDto, OrgChartRowDto, SeparationDto, TenantCatalogPermissionDto,
    TenantDirectoryRoleDto, TenantDirectoryUserDto, TenantFileAttachmentDto,
    TenantPermissionScopeDto,
};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::entities::d0007_employee_core::employee;
use crate::entities::d0008_document_system::employee_document;
use crate::entities::d0029_file_storage::file_storage;
use crate::resolvers::scope::{
    assert_employee_in_data_scope, data_scope_employee, employee_in_data_scope,
    require_any_exact_scope,
    require_employee_directory_read_all, require_employee_manage_all, require_exact_all_scope,
    require_exact_permission_scope, require_tenant_rbac_admin, resolve_viewer_employee,
};
use crate::services::{company_document_service, document_file_service};
use crate::services::{
    directory_service, document_service, employee_service, employment_history_service,
    offboarding_fnf_service, onboarding_service, org_service, profile_change_service,
    profile_extras_service, profile_record_service, rbac_admin_service, separation_service,
};

pub struct QueryRoot;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EmployeeTargetAccess {
    SelfBound,
    ScopedRead,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OnboardingReadAccess {
    SelfBound,
    ManageAll,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EmploymentHistoryReadAccess {
    SelfBound,
    TenantWide,
}

#[derive(Debug, Serialize, Deserialize)]
struct EmployeeDirectoryCursor {
    employee_code: String,
    id: Uuid,
}

fn employee_target_access(
    ctx: &Context<'_>,
    target_employee_id: Uuid,
) -> Result<EmployeeTargetAccess> {
    let claims = require_client_claims(ctx)?;
    data_scope_employee(ctx)?;
    employee_target_access_from_claims(claims, target_employee_id).ok_or_else(|| {
        KabiPayError::Forbidden(format!(
            "{PERM_EMPLOYEE_READ} permission requires an explicit valid scope"
        ))
        .into_graphql()
    })
}

fn employee_target_access_from_claims(
    claims: &kabipay_common::context::ClientClaims,
    target_employee_id: Uuid,
) -> Option<EmployeeTargetAccess> {
    if !claims.has_any_permission(&[PERM_EMPLOYEE_READ])
        || claims.scope_for_permission(PERM_EMPLOYEE_READ).is_none()
    {
        return None;
    }
    Some(if claims.employee_id == Some(target_employee_id) {
        EmployeeTargetAccess::SelfBound
    } else {
        EmployeeTargetAccess::ScopedRead
    })
}

async fn can_view_private_employee_profile(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<bool> {
    let claims = require_client_claims(ctx)?;
    match employee_target_access_from_claims(claims, target_employee_id) {
        Some(EmployeeTargetAccess::SelfBound) => Ok(true),
        Some(EmployeeTargetAccess::ScopedRead) => {
            employee_in_data_scope(ctx, db, tenant_id, target_employee_id).await
        }
        None => Ok(false),
    }
}

fn can_view_payroll_sensitive(ctx: &Context<'_>, target_employee_id: Uuid) -> bool {
    let Ok(claims) = require_client_claims(ctx) else {
        return false;
    };
    can_view_payroll_sensitive_from_claims(claims, target_employee_id)
}

fn can_view_payroll_sensitive_from_claims(
    claims: &kabipay_common::context::ClientClaims,
    target_employee_id: Uuid,
) -> bool {
    if !claims.has_any_permission(&[PERM_PAYROLL_READ]) {
        return false;
    }
    match claims.scope_for_permission(PERM_PAYROLL_READ) {
        Some(ScopeType::All) => true,
        Some(ScopeType::Self_) => claims.employee_id == Some(target_employee_id),
        _ => false,
    }
}

fn employment_history_read_access(
    ctx: &Context<'_>,
    target_employee_id: Uuid,
) -> Result<EmploymentHistoryReadAccess> {
    let claims = require_client_claims(ctx)?;
    let is_self_target = claims.employee_id == Some(target_employee_id);
    if !claims.has_any_permission(&[PERM_PAYROLL_READ]) {
        return Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_READ} permission required"
        ))
        .into_graphql());
    }
    match claims.scope_for_permission(PERM_PAYROLL_READ) {
        Some(ScopeType::All) => Ok(EmploymentHistoryReadAccess::TenantWide),
        Some(ScopeType::Self_) if is_self_target => Ok(EmploymentHistoryReadAccess::SelfBound),
        Some(ScopeType::Self_) => Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_READ} permission requires ALL scope"
        ))
        .into_graphql()),
        Some(_) if is_self_target => Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_READ} permission requires SELF or ALL scope"
        ))
        .into_graphql()),
        Some(_) => Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_READ} permission requires ALL scope"
        ))
        .into_graphql()),
        None => Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_READ} permission requires an explicit valid scope"
        ))
        .into_graphql()),
    }
}

async fn authorize_employee_target(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
    access: EmployeeTargetAccess,
) -> Result<()> {
    if access == EmployeeTargetAccess::ScopedRead {
        assert_employee_in_data_scope(ctx, db, tenant_id, target_employee_id).await?;
    }
    Ok(())
}

async fn load_scoped_employee_target_with_connection<C, T, Load, LoadFuture>(
    db: &C,
    tenant_id: Uuid,
    target_employee_id: Uuid,
    scope: ScopeType,
    viewer: Option<kabipay_common::context::ClientViewerEmployee>,
    load: Load,
) -> Result<Option<T>>
where
    C: ConnectionTrait + Sync,
    Load: FnOnce(Uuid) -> LoadFuture,
    LoadFuture: Future<Output = Result<Option<T>>>,
{
    let filter = resolve_employee_scope_filter_with_connection(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    if !filter.allows_employee(target_employee_id) {
        return Ok(None);
    }
    load(target_employee_id).await
}

fn merge_employee_scope_filters(
    current: EmployeeScopeFilter,
    next: EmployeeScopeFilter,
) -> EmployeeScopeFilter {
    match (current, next) {
        (EmployeeScopeFilter::Unrestricted, _) | (_, EmployeeScopeFilter::Unrestricted) => {
            EmployeeScopeFilter::Unrestricted
        }
        (EmployeeScopeFilter::Empty, filter) | (filter, EmployeeScopeFilter::Empty) => filter,
        (
            EmployeeScopeFilter::EmployeeIds(mut current_ids),
            EmployeeScopeFilter::EmployeeIds(next_ids),
        ) => {
            current_ids.extend(next_ids);
            current_ids.sort_unstable();
            current_ids.dedup();
            EmployeeScopeFilter::EmployeeIds(current_ids)
        }
    }
}

async fn employee_document_scope_filter(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> Result<EmployeeScopeFilter> {
    let claims = require_client_claims(ctx)?;
    let mut filter = EmployeeScopeFilter::Empty;
    let mut has_valid_grant = false;
    let mut invalid_permission = None;

    if claims.has_any_permission(&[PERM_EMPLOYEE_READ]) {
        if let Some(scope) = claims.scope_for_permission(PERM_EMPLOYEE_READ) {
            has_valid_grant = true;
            let viewer = if scope == ScopeType::All {
                None
            } else {
                resolve_viewer_employee(ctx, db, tenant_id).await?
            };
            let read_filter =
                resolve_employee_scope_filter_with_connection(db, tenant_id, scope, viewer)
                    .await
                    .map_err(KabiPayError::into_graphql)?;
            filter = merge_employee_scope_filters(filter, read_filter);
        } else {
            invalid_permission.get_or_insert(PERM_EMPLOYEE_READ);
        }
    }

    if has_valid_grant {
        return Ok(filter);
    }
    if let Some(permission) = invalid_permission {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires an explicit valid scope"
        ))
        .into_graphql());
    }
    Err(KabiPayError::Forbidden(format!(
        "{PERM_EMPLOYEE_READ} permission required"
    ))
    .into_graphql())
}

fn require_employee_record_access_candidate(ctx: &Context<'_>) -> Result<()> {
    require_any_exact_scope(ctx, &[PERM_EMPLOYEE_READ]).map(|_| ())
}

fn require_employee_reference_access(ctx: &Context<'_>) -> Result<()> {
    require_any_exact_scope(ctx, &[PERM_EMPLOYEE_READ]).map(|_| ())
}

fn onboarding_read_access(ctx: &Context<'_>) -> Result<OnboardingReadAccess> {
    let claims = require_client_claims(ctx)?;
    let manages = claims.has_any_permission(&[PERM_ONBOARDING_MANAGE]);
    if manages && claims.scope_for_permission(PERM_ONBOARDING_MANAGE) == Some(ScopeType::All) {
        return Ok(OnboardingReadAccess::ManageAll);
    }
    if claims.has_any_permission(&[PERM_ONBOARDING_SELF]) {
        require_exact_permission_scope(ctx, PERM_ONBOARDING_SELF)?;
        return Ok(OnboardingReadAccess::SelfBound);
    }
    if manages {
        require_exact_all_scope(ctx, PERM_ONBOARDING_MANAGE)?;
    }
    Err(KabiPayError::Forbidden(
        "onboarding:self or onboarding:manage permission required".into(),
    )
    .into_graphql())
}

fn onboarding_target_access(
    ctx: &Context<'_>,
    target_employee_id: Uuid,
) -> Result<OnboardingReadAccess> {
    let claims = require_client_claims(ctx)?;
    if claims.employee_id == Some(target_employee_id) {
        require_exact_permission_scope(ctx, PERM_ONBOARDING_SELF)?;
        Ok(OnboardingReadAccess::SelfBound)
    } else {
        require_exact_all_scope(ctx, PERM_ONBOARDING_MANAGE)?;
        Ok(OnboardingReadAccess::ManageAll)
    }
}

fn onboarding_employee_predicate(
    ctx: &Context<'_>,
    access: OnboardingReadAccess,
) -> Result<Option<Uuid>> {
    if access == OnboardingReadAccess::ManageAll {
        return Ok(None);
    }
    let claims = require_client_claims(ctx)?;
    claims.employee_id.map(Some).ok_or_else(|| {
        KabiPayError::Forbidden("onboarding:self requires a JWT-linked employee".into())
            .into_graphql()
    })
}

fn decode_employee_directory_cursor(raw: &str) -> Result<EmployeeDirectoryCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(raw.trim()).map_err(|_| {
        KabiPayError::Validation("invalid employee directory cursor".into()).into_graphql()
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        KabiPayError::Validation("invalid employee directory cursor".into()).into_graphql()
    })
}

fn encode_employee_directory_cursor(row: &employee::Model) -> Result<String> {
    let bytes = serde_json::to_vec(&EmployeeDirectoryCursor {
        employee_code: row.employee_code.clone(),
        id: row.id,
    })
    .map_err(|error| KabiPayError::from(error).into_graphql())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

async fn list_scoped_directory_page(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    limit: u64,
    after: Option<&str>,
) -> Result<(Vec<employee::Model>, Option<String>)> {
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, db, tenant_id).await?
    };
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.ne("TERMINATED"));
    match filter {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => return Ok((Vec::new(), None)),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => {
            return Ok((Vec::new(), None));
        }
        EmployeeScopeFilter::EmployeeIds(ids) => {
            query = query.filter(employee::Column::Id.is_in(ids));
        }
    }
    if let Some(raw) = after.filter(|value| !value.trim().is_empty()) {
        let cursor = decode_employee_directory_cursor(raw)?;
        query = query.filter(
            Condition::any()
                .add(employee::Column::EmployeeCode.gt(cursor.employee_code.clone()))
                .add(
                    Condition::all()
                        .add(employee::Column::EmployeeCode.eq(cursor.employee_code))
                        .add(employee::Column::Id.gt(cursor.id)),
                ),
        );
    }
    let limit = limit.clamp(1, 100);
    let mut rows = query
        .order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .map_err(|error: sea_orm::DbErr| KabiPayError::from(error).into_graphql())?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.pop();
    }
    let next_cursor = if has_more {
        rows.last()
            .map(encode_employee_directory_cursor)
            .transpose()?
    } else {
        None
    };
    Ok((rows, next_cursor))
}

async fn list_scoped_directory_hierarchy(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
) -> Result<Vec<employee::Model>> {
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, db, tenant_id).await?
    };
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.ne("TERMINATED"));
    match filter {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(Vec::new()),
        EmployeeScopeFilter::EmployeeIds(ids) => {
            query = query.filter(employee::Column::Id.is_in(ids));
        }
    }
    query
        .order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
        .all(db)
        .await
        .map_err(|error: sea_orm::DbErr| KabiPayError::from(error).into_graphql())
}

#[Object]
impl QueryRoot {
    /// Liveness probe for this federated subgraph. Always returns `ok`.
    async fn employee_health(&self) -> &'static str {
        "ok"
    }

    /// Company directory protected by exact `employee_directory:read=ALL` authority.
    async fn employee_directory_page(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        after: Option<String>,
    ) -> Result<EmployeeDirectoryPageDto> {
        require_employee_directory_read_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let (page_rows, next_cursor) =
            list_scoped_directory_page(ctx, &db, tenant_id, ScopeType::All, limit, after.as_deref())
                .await?;
        let rows = enrich_directory_entries(&db, tenant_id, page_rows).await?;
        Ok(EmployeeDirectoryPageDto {
            has_more: next_cursor.is_some(),
            next_cursor,
            rows,
        })
    }

    /// Reporting hierarchy limited by exact `employee_directory:read=ALL`.
    async fn organization_directory_chart(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<EmployeeDirectoryEntryDto>> {
        require_employee_directory_read_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = list_scoped_directory_hierarchy(ctx, &db, tenant_id, ScopeType::All).await?;
        enrich_directory_entries(&db, tenant_id, rows).await
    }

    /// Public profile projection plus server-derived private/edit capabilities.
    async fn employee_profile_access(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Option<EmployeeProfileAccessDto>> {
        require_employee_directory_read_all(ctx)?;
        let target_id = parse_uuid(&employee_id, "employeeId")?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let Some(target) = directory_service::find_current_by_id(&db, tenant_id, target_id)
            .await
            .map_err(KabiPayError::into_graphql)?
        else {
            return Ok(None);
        };
        let claims = require_client_claims(ctx)?;
        let is_self = claims.employee_id == Some(target_id);
        let can_manage = require_employee_manage_all(ctx).is_ok();
        let can_view_private =
            can_view_private_employee_profile(ctx, &db, tenant_id, target_id).await?;
        let can_view_payroll = can_view_payroll_sensitive(ctx, target_id);
        let mut entries = enrich_directory_entries(&db, tenant_id, vec![target]).await?;
        let directory_entry = entries.pop().ok_or_else(|| {
            KabiPayError::Internal("profile directory entry missing".into()).into_graphql()
        })?;
        Ok(Some(EmployeeProfileAccessDto {
            directory_entry,
            is_self,
            can_view_private_profile: can_view_private,
            can_view_payroll_sensitive: can_view_payroll,
            can_edit_personal_profile: is_self || can_manage,
            can_manage_organization_fields: can_manage,
            can_review_profile_changes: can_manage,
        }))
    }

    async fn employee_profile_change_requests(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        status: Option<String>,
    ) -> Result<Vec<EmployeeProfileChangeRequestDto>> {
        let target_id = parse_uuid(&employee_id, "employeeId")?;
        let access = employee_target_access(ctx, target_id)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        authorize_employee_target(ctx, &db, tenant_id, target_id, access).await?;
        let rows = profile_change_service::list_requests(
            &db,
            tenant_id,
            target_id,
            status.as_deref(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(EmployeeProfileChangeRequestDto::from).collect())
    }

    /// HR-only masked queue. Sensitive values require the separate detail query.
    async fn employee_profile_review_queue(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        #[graphql(default = 50)] limit: i32,
    ) -> Result<Vec<EmployeeProfileReviewQueueItemDto>> {
        require_employee_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = profile_change_service::list_review_queue(
            &db,
            tenant_id,
            status.as_deref().or(Some("PENDING")),
            (limit.clamp(1, 100) as u64).saturating_mul(4),
            None,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let employee_ids = rows.iter().map(|row| row.employee_id).collect::<Vec<_>>();
        let employees = employee_service::find_by_ids(&db, tenant_id, &employee_ids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let employee_map = employees.into_iter().map(|row| (row.id, row)).collect::<std::collections::HashMap<_, _>>();
        let mut output = Vec::new();
        for row in rows {
            let Some(employee) = employee_map.get(&row.employee_id) else { continue; };
            let has_supporting_document = row.supporting_document_id.is_some();
            output.push(EmployeeProfileReviewQueueItemDto {
                request: EmployeeProfileChangeRequestDto::from(row),
                employee_code: employee.employee_code.clone(),
                employee_name: format!("{} {}", employee.first_name, employee.last_name).trim().to_string(),
                has_supporting_document,
            });
            if output.len() >= limit.clamp(1, 100) as usize { break; }
        }
        Ok(output)
    }

    /// HR-only, scope-checked request detail containing decrypted current and proposed values.
    async fn employee_profile_change_review_detail(
        &self,
        ctx: &Context<'_>,
        request_id: ID,
    ) -> Result<EmployeeProfileChangeReviewDetailDto> {
        require_employee_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let request_id = parse_uuid(&request_id, "requestId")?;
        let request = profile_change_service::find_request(&db, tenant_id, request_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "employeeProfileChangeRequest", id: request_id.to_string() }.into_graphql())?;
        if request.status != "PENDING" {
            return Err(KabiPayError::Conflict("protected values are available only while a request is pending".into()).into_graphql());
        }
        let employee = employee_service::find_by_id(&db, tenant_id, request.employee_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "employee", id: request.employee_id.to_string() }.into_graphql())?;
        let (current_values, requested_values) = profile_change_service::review_values(&db, &request)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeProfileChangeReviewDetailDto {
            request: EmployeeProfileChangeRequestDto::from(request),
            employee_code: employee.employee_code,
            employee_name: format!("{} {}", employee.first_name, employee.last_name).trim().to_string(),
            current_values: async_graphql::Json(current_values),
            requested_values: async_graphql::Json(requested_values),
        })
    }

    async fn employee_evidence_review_queue(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: i32,
    ) -> Result<Vec<EmployeeEvidenceReviewQueueItemDto>> {
        require_employee_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let records = profile_record_service::list_pending_evidence_reviews(&db, tenant_id, limit.clamp(1, 100) as u64, None)
            .await.map_err(KabiPayError::into_graphql)?;
        let employee_ids = records.iter().map(|row| row.employee_id()).collect::<Vec<_>>();
        let employees = employee_service::find_by_ids(&db, tenant_id, &employee_ids).await.map_err(KabiPayError::into_graphql)?;
        let employee_map = employees.into_iter().map(|row| (row.id, row)).collect::<std::collections::HashMap<_, _>>();
        let education_ids = records.iter().filter_map(|row| match row { profile_record_service::PendingEvidenceRecord::Education(row) => Some(row.id), _ => None }).collect::<Vec<_>>();
        let work_ids = records.iter().filter_map(|row| match row { profile_record_service::PendingEvidenceRecord::Work(row) => Some(row.id), _ => None }).collect::<Vec<_>>();
        let education_evidence = profile_record_service::education_evidence_ids_by_record(&db, tenant_id, &education_ids).await.map_err(KabiPayError::into_graphql)?;
        let work_evidence = profile_record_service::work_evidence_ids_by_record(&db, tenant_id, &work_ids).await.map_err(KabiPayError::into_graphql)?;
        let mut output = Vec::new();
        for record in records {
            let Some(employee) = employee_map.get(&record.employee_id()) else { continue; };
            let (evidence_type, summary, evidence_ids) = match &record {
                profile_record_service::PendingEvidenceRecord::Education(row) => (
                    "EDUCATION".to_string(), format!("{} · {}", row.qualification, row.institution), education_evidence.get(&row.id).cloned().unwrap_or_default()),
                profile_record_service::PendingEvidenceRecord::Work(row) => (
                    "WORK_EXPERIENCE".to_string(), format!("{} · {}", row.role_title, row.company), work_evidence.get(&row.id).cloned().unwrap_or_default()),
            };
            output.push(EmployeeEvidenceReviewQueueItemDto {
                record_id: ID(record.record_id().to_string()), employee_id: ID(employee.id.to_string()),
                employee_code: employee.employee_code.clone(), employee_name: format!("{} {}", employee.first_name, employee.last_name).trim().to_string(),
                evidence_type, summary, evidence_document_ids: evidence_ids.into_iter().map(|id| ID(id.to_string())).collect(), created_at: record.created_at(),
            });
        }
        Ok(output)
    }

    async fn employee_education_records(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Vec<EmployeeEducationDto>> {
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        let access = employee_target_access(ctx, employee_id)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        authorize_employee_target(ctx, &db, tenant_id, employee_id, access).await?;
        let rows = profile_record_service::list_education(&db, tenant_id, employee_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let record_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let evidence_by_record = profile_record_service::education_evidence_ids_by_record(
            &db, tenant_id, &record_ids,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let mut output = Vec::with_capacity(rows.len());
        for row in rows {
            let evidence_ids = evidence_by_record.get(&row.id).cloned().unwrap_or_default();
            output.push(EmployeeEducationDto::from_model(row, evidence_ids));
        }
        Ok(output)
    }

    async fn employee_work_experience_records(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Vec<EmployeeWorkExperienceDto>> {
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        let access = employee_target_access(ctx, employee_id)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        authorize_employee_target(ctx, &db, tenant_id, employee_id, access).await?;
        let rows = profile_record_service::list_work_experience(&db, tenant_id, employee_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let record_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let evidence_by_record = profile_record_service::work_evidence_ids_by_record(
            &db, tenant_id, &record_ids,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let mut output = Vec::with_capacity(rows.len());
        for row in rows {
            let evidence_ids = evidence_by_record.get(&row.id).cloned().unwrap_or_default();
            output.push(EmployeeWorkExperienceDto::from_model(row, evidence_ids));
        }
        Ok(output)
    }

    /// Fetch one employee by UUID inside the caller's tenant.
    ///
    /// Returns `null` if the employee does not exist, is soft-deleted, or
    /// belongs to another tenant (never leaks cross-tenant rows).
    async fn employee(&self, ctx: &Context<'_>, id: ID) -> Result<Option<EmployeeDto>> {
        resolve_employee_dto(ctx, id).await
    }

    /// Authoritative employee profile linked to the authenticated user.
    ///
    /// This resolves `employee.user_id` on every call instead of trusting the
    /// optional denormalized `employee_id` access-token claim, so older tokens
    /// and repaired account links still reach the correct profile.
    async fn my_employee(&self, ctx: &Context<'_>) -> Result<Option<EmployeeDto>> {
        data_scope_employee(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let model = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::UserId.eq(claims.sub))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(&db)
            .await
            .map_err(|error| KabiPayError::from(error).into_graphql())?;

        let Some(model) = model else {
            return Ok(None);
        };
        let mut enriched =
            enrich_employee_dtos(&db, tenant_id, vec![EmployeeDto::from(model)]).await?;
        Ok(enriched.pop())
    }

    /// Apollo Federation **entity** lookup (`_entities`) — not exposed as a public `Query` field.
    /// Enables `type Employee @key(fields: "id")` in the subgraph SDL (**M9**).
    #[graphql(entity)]
    async fn find_employee_by_id(&self, ctx: &Context<'_>, id: ID) -> Result<Option<EmployeeDto>> {
        resolve_employee_dto(ctx, id).await
    }

    /// List the first `limit` employees in the caller's tenant (capped at 100).
    async fn employees(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 20)] limit: u64,
    ) -> Result<Vec<EmployeeDto>> {
        let scope = data_scope_employee(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = if scope == ScopeType::All {
            None
        } else {
            resolve_viewer_employee(ctx, &db, tenant_id).await?
        };
        let models = employee_service::list(&db, tenant_id, limit, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let dtos: Vec<EmployeeDto> = models.into_iter().map(EmployeeDto::from).collect();
        enrich_employee_dtos(&db, tenant_id, dtos).await
    }

    /// Salary-bearing employment history, newest first.
    ///
    /// Access is limited to exact `payroll:read=SELF` for the JWT-linked employee or
    /// exact `payroll:read=ALL` for tenant payroll/HR/admin users. Employee directory
    /// and manager team scope never grant salary access.
    async fn employment_history_records(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        #[graphql(default = 24)] limit: u64,
    ) -> Result<Vec<EmploymentHistoryRecordDto>> {
        let eid = parse_uuid(&employee_id, "employeeId")?;
        employment_history_read_access(ctx, eid)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = employment_history_service::list_for_employee(&db, tenant_id, eid, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(EmploymentHistoryRecordDto::from).collect())
    }

    /// Master list of document / policy types defined for the tenant.
    async fn document_types(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<DocumentTypeDto>> {
        require_employee_reference_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = document_service::list_document_types(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(DocumentTypeDto::from).collect())
    }

    /// Company policy, onboarding, and exit-formality documents.
    async fn company_documents(
        &self,
        ctx: &Context<'_>,
        category: Option<String>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<CompanyDocumentDto>> {
        let access = onboarding_read_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let include_hidden = access == OnboardingReadAccess::ManageAll;
        let rows = company_document_service::list_company_documents(
            &db,
            tenant_id,
            category,
            active_only,
            include_hidden,
            limit,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let file_map = company_document_service::map_file_storage_rows(&db, tenant_id, &rows)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let file = file_map.get(&row.file_storage_id);
                CompanyDocumentDto::from(row).with_file(file)
            })
            .collect())
    }

    /// Uploaded employee documents. Omit `employeeId` to list the caller's own files (JWT).
    async fn employee_documents(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<EmployeeDocumentDto>> {
        let requested_target = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employee id"))
            .transpose()?;
        let target_access = match requested_target {
            Some(employee_id) => Some(employee_target_access(ctx, employee_id)?),
            None => {
                data_scope_employee(ctx)?;
                None
            }
        };
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(eid) = requested_target {
            let access = target_access.expect("explicit target must have an access decision");
            authorize_employee_target(ctx, &db, tenant_id, eid, access).await?;
            eid
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        let rows = document_service::list_employee_documents(&db, tenant_id, emp, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let mut fs_ids: Vec<Uuid> = rows.iter().filter_map(|r| r.file_storage_id).collect();
        fs_ids.sort_unstable();
        fs_ids.dedup();
        let mut dt_ids: Vec<Uuid> = rows.iter().map(|r| r.document_type_id).collect();
        dt_ids.sort_unstable();
        dt_ids.dedup();
        let fs_map = document_service::map_file_storage_rows(&db, tenant_id, &fs_ids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let dt_map = document_service::map_document_type_rows(&db, tenant_id, &dt_ids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|m| {
                let fs = m.file_storage_id.and_then(|id| fs_map.get(&id));
                let dt = dt_map.get(&m.document_type_id);
                EmployeeDocumentDto::from(m).with_file_and_type(
                    fs.and_then(|f| f.original_filename.clone()),
                    fs.and_then(|f| f.mime_type.clone()),
                    fs.and_then(|f| f.uploaded_by),
                    dt.map(|d| d.name.clone()),
                    dt.and_then(|d| d.category.clone()),
                )
            })
            .collect())
    }

    /// Primary bank account for payroll (masked account number in API).
    async fn employee_primary_bank(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Option<EmployeeBankAccountDto>> {
        let eid = parse_uuid(&employee_id, "employeeId")?;
        let access = employee_target_access(ctx, eid)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        authorize_employee_target(ctx, &db, tenant_id, eid, access).await?;
        let row = profile_extras_service::find_primary_bank(&db, tenant_id, eid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(row.as_ref().map(EmployeeBankAccountDto::from_model))
    }

    /// Masked PAN / Aadhaar primary rows for the employee profile.
    async fn employee_identity_profile(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<EmployeeIdentityProfileDto> {
        let eid = parse_uuid(&employee_id, "employeeId")?;
        let access = employee_target_access(ctx, eid)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        authorize_employee_target(ctx, &db, tenant_id, eid, access).await?;
        let pan = profile_extras_service::find_primary_pan(&db, tenant_id, eid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let aadhaar = profile_extras_service::find_primary_aadhaar(&db, tenant_id, eid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeIdentityProfileDto {
            pan: pan.as_ref().map(EmployeePanRecordDto::from_model),
            aadhaar: aadhaar.as_ref().map(EmployeeAadhaarRecordDto::from_model),
        })
    }
    async fn departments(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<DepartmentDto>> {
        require_employee_reference_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = org_service::list_departments(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(DepartmentDto::from).collect())
    }

    /// Job titles / designations in the tenant. Excludes soft-deleted rows.
    async fn designations(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<DesignationDto>> {
        require_employee_reference_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = org_service::list_designations(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(DesignationDto::from).collect())
    }

    /// Tenant roles for assigning **ROLE**-scoped expense policies (`expense:manage` etc.).
    /// Unlike [`Self::tenant_directory_roles`], this does not require **`role:manage`**.
    async fn expense_assignable_roles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TenantDirectoryRoleDto>> {
        require_exact_all_scope(ctx, PERM_EXPENSE_MANAGE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = rbac_admin_service::list_roles(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantDirectoryRoleDto::from).collect())
    }

    /// Reporting hierarchy as a **flat** list (`reportingManagerId` → parent). Build a tree in the client.
    /// Respects the same **`employee`** `resource_scopes` as **`employees`** (SELF / TEAM / DEPARTMENT / ALL).
    async fn org_chart(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 500)] limit: u64,
    ) -> Result<Vec<OrgChartRowDto>> {
        let scope = data_scope_employee(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = if scope == ScopeType::All {
            None
        } else {
            resolve_viewer_employee(ctx, &db, tenant_id).await?
        };
        let models = employee_service::list_for_org_chart(&db, tenant_id, limit, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;

        let mut dept_ids: Vec<Uuid> = models.iter().filter_map(|e| e.department_id).collect();
        dept_ids.sort_unstable();
        dept_ids.dedup();
        let mut desig_ids: Vec<Uuid> = models.iter().filter_map(|e| e.designation_id).collect();
        desig_ids.sort_unstable();
        desig_ids.dedup();

        let dept_map = org_service::map_department_names(&db, tenant_id, &dept_ids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let desig_map = org_service::map_designation_titles(&db, tenant_id, &desig_ids)
            .await
            .map_err(KabiPayError::into_graphql)?;

        let rows = models
            .into_iter()
            .map(|m| {
                let full_name = format!("{} {}", m.first_name.trim(), m.last_name.trim())
                    .trim()
                    .to_string();
                OrgChartRowDto {
                    employee_id: ID(m.id.to_string()),
                    employee_code: m.employee_code,
                    full_name,
                    reporting_manager_id: m.reporting_manager_id.map(|u| ID(u.to_string())),
                    department_name: m.department_id.and_then(|id| dept_map.get(&id).cloned()),
                    designation_title: m.designation_id.and_then(|id| desig_map.get(&id).cloned()),
                }
            })
            .collect();
        Ok(rows)
    }

    /// Private employee document bytes. Caller must be able to read the employee who owns the document.
    async fn employee_document_attachment(
        &self,
        ctx: &Context<'_>,
        employee_document_id: ID,
    ) -> Result<EmployeeDocumentAttachmentDto> {
        require_employee_record_access_candidate(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let doc_id = parse_uuid(&employee_document_id, "employeeDocumentId")?;
        let scope_filter = employee_document_scope_filter(ctx, &db, tenant_id).await?;
        let model = document_service::find_employee_document_in_scope(
            &db,
            tenant_id,
            doc_id,
            &scope_filter,
        )
        .await
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| {
            KabiPayError::NotFound {
                entity: "employeeDocument",
                id: doc_id.to_string(),
            }
            .into_graphql()
        })?;
        let file_id = model.file_storage_id.ok_or_else(|| {
            KabiPayError::Validation("document has no file yet".to_string()).into_graphql()
        })?;
        let fs_row = file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "employeeDocument",
                    id: doc_id.to_string(),
                }
                .into_graphql()
            })?;
        let bytes = document_file_service::read_stored_file_bytes(
            &document_file_service::local_file_root(),
            &fs_row,
        )
        .await
        .map_err(|error| match error {
            KabiPayError::NotFound { .. } => KabiPayError::NotFound {
                entity: "employeeDocument",
                id: doc_id.to_string(),
            }
            .into_graphql(),
            other => other.into_graphql(),
        })?;
        Ok(EmployeeDocumentAttachmentDto {
            file_name: fs_row
                .original_filename
                .clone()
                .unwrap_or_else(|| "document".to_string()),
            mime_type: fs_row
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            file_size_bytes: fs_row
                .file_size_bytes
                .and_then(|size| i32::try_from(size).ok()),
            content_base64: STANDARD.encode(bytes),
        })
    }

    /// Private company document bytes authorized through the company document record.
    async fn company_document_attachment(
        &self,
        ctx: &Context<'_>,
        company_document_id: ID,
    ) -> Result<TenantFileAttachmentDto> {
        let access = onboarding_read_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let document_id = parse_uuid(&company_document_id, "companyDocumentId")?;
        let doc = company_document_service::find_visible_company_document(
            &db,
            tenant_id,
            document_id,
            access == OnboardingReadAccess::ManageAll,
        )
        .await
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| {
            KabiPayError::NotFound {
                entity: "companyDocument",
                id: document_id.to_string(),
            }
            .into_graphql()
        })?;
        let fs_row = file_storage::Entity::find_by_id(doc.file_storage_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::Internal("company document file metadata is missing".into())
                    .into_graphql()
            })?;
        let bytes = document_file_service::read_stored_file_bytes(
            &document_file_service::local_file_root(),
            &fs_row,
        )
        .await
        .map_err(|error| match error {
            KabiPayError::NotFound { .. } => KabiPayError::NotFound {
                entity: "companyDocument",
                id: document_id.to_string(),
            }
            .into_graphql(),
            other => other.into_graphql(),
        })?;
        Ok(TenantFileAttachmentDto {
            file_name: fs_row
                .original_filename
                .clone()
                .unwrap_or_else(|| doc.title.clone()),
            mime_type: fs_row
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            file_size_bytes: fs_row
                .file_size_bytes
                .and_then(|size| i32::try_from(size).ok()),
            content_base64: STANDARD.encode(bytes),
        })
    }

    /// Onboarding tasks for an employee. Omit `employeeId` for the JWT subject's checklist.
    /// HR / directory managers may pass another employee id (same data-scope rules as documents).
    async fn onboarding_checklist(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<OnboardingChecklistItemDto>> {
        let requested_target = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employee id"))
            .transpose()?;
        let target_access = requested_target
            .map(|employee_id| onboarding_target_access(ctx, employee_id))
            .transpose()?;
        if requested_target.is_none() {
            require_exact_permission_scope(ctx, PERM_ONBOARDING_SELF)?;
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(eid) = requested_target {
            if target_access != Some(OnboardingReadAccess::ManageAll) {
                let viewer = resolve_client_employee_id(ctx, &db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)?;
                if viewer != eid {
                    return Err(KabiPayError::Forbidden(
                        "onboarding:self is limited to the JWT-linked employee".into(),
                    )
                    .into_graphql());
                }
            }
            eid
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        let rows = onboarding_service::list_checklist_for_employee(&db, tenant_id, emp, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(OnboardingChecklistItemDto::from).collect())
    }

    /// Separation / offboarding requests. `onboarding:manage` sees tenant-wide rows;
    /// `onboarding:self` (or manage) sees **their own**; otherwise forbidden.
    async fn separations(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<SeparationDto>> {
        let access = onboarding_read_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = onboarding_employee_predicate(ctx, access)?;
        let rows = separation_service::list_for_tenant(&db, tenant_id, limit, filter)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(SeparationDto::from).collect())
    }

    /// Full & final row for a separation (if HR has run approval, a DRAFT or PROCESSED row exists).
    async fn fnf_settlement(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<Option<FnfSettlementDto>> {
        let access = onboarding_read_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let sid = parse_uuid(&separation_id, "separation id")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_predicate = onboarding_employee_predicate(ctx, access)?;
        let m = offboarding_fnf_service::get_visible_fnf_by_separation(
            &db,
            tenant_id,
            sid,
            employee_predicate,
        )
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(m.map(FnfSettlementDto::from))
    }

    /// Department exit clearance items for a separation.
    async fn clearance_checklist(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<Vec<ClearanceChecklistItemDto>> {
        let access = onboarding_read_access(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let sid = parse_uuid(&separation_id, "separation id")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_predicate = onboarding_employee_predicate(ctx, access)?;
        let rows = offboarding_fnf_service::list_visible_clearance(
            &db,
            tenant_id,
            sid,
            employee_predicate,
        )
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ClearanceChecklistItemDto::from).collect())
    }

    /// Tenant users for RBAC assignment (`role:manage` / HR admin).
    async fn tenant_directory_users(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TenantDirectoryUserDto>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = rbac_admin_service::list_users(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantDirectoryUserDto::from).collect())
    }

    /// Tenant-defined roles.
    async fn tenant_directory_roles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TenantDirectoryRoleDto>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = rbac_admin_service::list_roles(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantDirectoryRoleDto::from).collect())
    }

    /// Permission catalog rows in the tenant schema (for matrix editing).
    async fn tenant_catalog_permissions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 300)] limit: u64,
    ) -> Result<Vec<TenantCatalogPermissionDto>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = rbac_admin_service::list_permissions(&db, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantCatalogPermissionDto::from).collect())
    }

    /// Permission UUIDs granted to a role (`role_permission`).
    async fn permission_ids_for_role(&self, ctx: &Context<'_>, role_id: ID) -> Result<Vec<ID>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rid = parse_uuid(&role_id, "roleId")?;
        let ids = rbac_admin_service::permission_ids_for_role(&db, tenant_id, rid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(ids.into_iter().map(|u| ID(u.to_string())).collect())
    }

    /// Role UUIDs assigned to a user (`user_role`).
    async fn role_ids_for_user(&self, ctx: &Context<'_>, user_id: ID) -> Result<Vec<ID>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uid = parse_uuid(&user_id, "userId")?;
        let ids = rbac_admin_service::role_ids_for_user(&db, tenant_id, uid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(ids.into_iter().map(|u| ID(u.to_string())).collect())
    }

    /// Data scopes (`permission_scope`) for list filtering (employee / leave / expense / …).
    async fn permission_scopes_for_role(
        &self,
        ctx: &Context<'_>,
        role_id: ID,
    ) -> Result<Vec<TenantPermissionScopeDto>> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rid = parse_uuid(&role_id, "roleId")?;
        let rows = rbac_admin_service::scopes_for_role(&db, tenant_id, rid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantPermissionScopeDto::from).collect())
    }
}

async fn enrich_directory_entries(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    rows: Vec<crate::entities::d0007_employee_core::employee::Model>,
) -> Result<Vec<EmployeeDirectoryEntryDto>> {
    let mut department_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.department_id).collect();
    department_ids.sort_unstable();
    department_ids.dedup();
    let mut designation_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.designation_id).collect();
    designation_ids.sort_unstable();
    designation_ids.dedup();
    let mut manager_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| row.reporting_manager_id)
        .collect();
    manager_ids.sort_unstable();
    manager_ids.dedup();

    let department_map = org_service::map_department_names(db, tenant_id, &department_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let designation_map = org_service::map_designation_titles(db, tenant_id, &designation_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let manager_map = employee_service::map_full_names(db, tenant_id, &manager_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            EmployeeDirectoryEntryDto::from_model(
                row,
                &department_map,
                &designation_map,
                &manager_map,
            )
        })
        .collect())
}

async fn resolve_employee_dto(ctx: &Context<'_>, id: ID) -> Result<Option<EmployeeDto>> {
    let employee_id = parse_uuid(&id, "employee id")?;
    let scope = data_scope_employee(ctx)?;
    let tenant_id = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant_id).await?;
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, &db, tenant_id).await?
    };
    let load_db = &db;
    let model = load_scoped_employee_target_with_connection(
        &db,
        tenant_id,
        employee_id,
        scope,
        viewer,
        |target_employee_id| async move {
            employee_service::find_by_id(load_db, tenant_id, target_employee_id)
                .await
                .map_err(KabiPayError::into_graphql)
        },
    )
    .await?;
    Ok(match model {
        Some(m) => {
            let dto = EmployeeDto::from(m);
            let mut enriched = enrich_employee_dtos(&db, tenant_id, vec![dto]).await?;
            enriched.pop()
        }
        None => None,
    })
}

pub(crate) async fn enrich_employee_dtos(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    dtos: Vec<EmployeeDto>,
) -> Result<Vec<EmployeeDto>> {
    fn push_uuid(ids: &mut Vec<Uuid>, id: &Option<ID>) {
        if let Some(u) = id.as_ref().and_then(|raw| Uuid::parse_str(raw.as_str()).ok()) {
            ids.push(u);
        }
    }
    fn dedup_sort(ids: &mut Vec<Uuid>) {
        ids.sort_unstable();
        ids.dedup();
    }

    let mut dept_ids = Vec::new();
    let mut desig_ids = Vec::new();
    let mut user_ids = Vec::new();
    let mut mgr_ids = Vec::new();
    for d in &dtos {
        push_uuid(&mut dept_ids, &d.department_id);
        push_uuid(&mut desig_ids, &d.designation_id);
        push_uuid(&mut user_ids, &d.user_id);
        push_uuid(&mut mgr_ids, &d.reporting_manager_id);
    }
    dedup_sort(&mut dept_ids);
    dedup_sort(&mut desig_ids);
    dedup_sort(&mut user_ids);
    dedup_sort(&mut mgr_ids);

    let dept_map = org_service::map_department_names(db, tenant_id, &dept_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let desig_map = org_service::map_designation_titles(db, tenant_id, &desig_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let user_map = rbac_admin_service::map_user_login_labels_by_ids(db, tenant_id, &user_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let mgr_map = employee_service::map_full_names(db, tenant_id, &mgr_ids)
        .await
        .map_err(KabiPayError::into_graphql)?;

    Ok(dtos
        .into_iter()
        .map(|d| d.with_reference_labels(&dept_map, &desig_map, &user_map, &mgr_map))
        .collect())
}

fn parse_uuid(raw: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(raw.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use chrono::{NaiveDate, Utc};
    use kabipay_common::context::{
        ClientClaims, ClientViewerEmployee, CLIENT_JWT_ISSUER, EMPLOYMENT_STATUS_ACTIVE,
        EMPLOYMENT_STATUS_PROBATION, PERM_EMPLOYEE_DIRECTORY_READ, PERM_EMPLOYEE_MANAGE,
        PERM_EMPLOYEE_READ, PERM_EXPENSE_MANAGE,
        PERM_NOTIFICATION_READ, PERM_ONBOARDING_MANAGE, PERM_ONBOARDING_SELF, PERM_PAYROLL_READ,
        PERM_ROLE_MANAGE,
    };
    use kabipay_common::subgraph::TenantId;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
    };
    use std::cell::Cell;
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct QueueProxy {
        query_results: Mutex<VecDeque<Vec<ProxyRow>>>,
        statements: Arc<Mutex<Vec<Statement>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for QueueProxy {
        async fn query(&self, statement: Statement) -> std::result::Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("query statement recorder lock")
                .push(statement);
            Ok(self
                .query_results
                .lock()
                .expect("proxy query result queue lock")
                .pop_front()
                .unwrap_or_default())
        }

        async fn execute(
            &self,
            _statement: Statement,
        ) -> std::result::Result<ProxyExecResult, DbErr> {
            Err(DbErr::Custom(
                "read authorization test unexpectedly executed a write".into(),
            ))
        }
    }

    async fn proxy_database(
        query_results: Vec<Vec<ProxyRow>>,
    ) -> (DatabaseConnection, Arc<Mutex<Vec<Statement>>>) {
        let statements = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(QueueProxy {
                query_results: Mutex::new(query_results.into()),
                statements: Arc::clone(&statements),
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (db, statements)
    }

    fn id_rows(ids: &[Uuid]) -> Vec<ProxyRow> {
        ids.iter()
            .copied()
            .map(|id| ProxyRow::new(BTreeMap::from([("id".into(), id.into())])))
            .collect()
    }

    fn normalized_sql(statement: &Statement) -> String {
        statement.sql.replace('"', "")
    }

    fn employee_document_row(model: &employee_document::Model) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("employee_id".into(), model.employee_id.into()),
            ("document_type_id".into(), model.document_type_id.into()),
            ("file_storage_id".into(), model.file_storage_id.into()),
            ("status".into(), model.status.clone().into()),
            ("expiry_date".into(), model.expiry_date.into()),
            ("workflow_instance_id".into(), model.workflow_instance_id.into()),
            ("uploaded_at".into(), model.uploaded_at.into()),
            ("verified_by".into(), model.verified_by.into()),
            ("verified_at".into(), model.verified_at.into()),
            ("is_deleted".into(), model.is_deleted.into()),
            ("deleted_at".into(), model.deleted_at.into()),
            ("deleted_by".into(), model.deleted_by.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    fn company_document_row(
        model: &crate::entities::d0056_company_documents::company_document::Model,
    ) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("category".into(), model.category.clone().into()),
            ("title".into(), model.title.clone().into()),
            ("description".into(), model.description.clone().into()),
            ("file_storage_id".into(), model.file_storage_id.into()),
            ("status".into(), model.status.clone().into()),
            (
                "visible_to_employees".into(),
                model.visible_to_employees.into(),
            ),
            ("uploaded_by".into(), model.uploaded_by.into()),
            ("is_deleted".into(), model.is_deleted.into()),
            ("deleted_at".into(), model.deleted_at.into()),
            ("deleted_by".into(), model.deleted_by.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    fn separation_row(
        model: &crate::entities::d0017_onboarding_offboarding::separation::Model,
    ) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("employee_id".into(), model.employee_id.into()),
            ("separation_type".into(), model.separation_type.clone().into()),
            ("resignation_date".into(), model.resignation_date.into()),
            ("last_working_date".into(), model.last_working_date.into()),
            ("reason".into(), model.reason.clone().into()),
            ("status".into(), model.status.clone().into()),
            ("approved_by".into(), model.approved_by.into()),
            ("workflow_instance_id".into(), model.workflow_instance_id.into()),
            ("offboarded_at".into(), model.offboarded_at.into()),
            ("offboarding_event_id".into(), model.offboarding_event_id.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    fn fnf_row(
        model: &crate::entities::d0017_onboarding_offboarding::fnf_settlement::Model,
    ) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("separation_id".into(), model.separation_id.into()),
            ("leave_encashment".into(), model.leave_encashment.into()),
            ("gratuity_amount".into(), model.gratuity_amount.into()),
            ("bonus_payable".into(), model.bonus_payable.into()),
            ("recovery_amount".into(), model.recovery_amount.into()),
            ("net_payable".into(), model.net_payable.into()),
            ("status".into(), model.status.clone().into()),
            ("processed_at".into(), model.processed_at.into()),
            ("processed_by".into(), model.processed_by.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    fn clearance_row(
        model: &crate::entities::d0017_onboarding_offboarding::clearance_checklist::Model,
    ) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("separation_id".into(), model.separation_id.into()),
            ("department".into(), model.department.clone().into()),
            ("task_name".into(), model.task_name.clone().into()),
            ("is_cleared".into(), model.is_cleared.into()),
            ("cleared_by".into(), model.cleared_by.into()),
            ("cleared_at".into(), model.cleared_at.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    fn claims(
        permission: &str,
        scope: Option<&str>,
        employee_id: Option<Uuid>,
    ) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| HashMap::from([(permission.to_string(), scope.to_string())]))
            .unwrap_or_default();
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id,
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute_query(claims: ClientClaims, query: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(query))
            .await
    }

    fn assert_forbidden_before_db(response: &async_graphql::Response, permission: &str) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission) && message.to_ascii_lowercase().contains("permission"),
            "unexpected authorization error: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.to_ascii_lowercase().contains("database"));
    }

    fn assert_authorized_before_db(response: &async_graphql::Response) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert_eq!(message, "internal server error");
    }

    fn sibling_permission(required: &str) -> &'static str {
        match required {
            PERM_EMPLOYEE_DIRECTORY_READ => PERM_EMPLOYEE_READ,
            PERM_EMPLOYEE_READ => PERM_EMPLOYEE_DIRECTORY_READ,
            PERM_EMPLOYEE_MANAGE => PERM_EMPLOYEE_READ,
            PERM_EXPENSE_MANAGE => PERM_EMPLOYEE_MANAGE,
            PERM_ONBOARDING_SELF => PERM_EMPLOYEE_READ,
            PERM_ONBOARDING_MANAGE => PERM_ONBOARDING_SELF,
            PERM_PAYROLL_READ => PERM_EMPLOYEE_READ,
            PERM_ROLE_MANAGE => PERM_EMPLOYEE_MANAGE,
            _ => PERM_NOTIFICATION_READ,
        }
    }

    #[test]
    fn private_profile_access_candidate_requires_employee_read_and_distinguishes_target() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();

        assert_eq!(
            employee_target_access_from_claims(
                &claims(PERM_EMPLOYEE_MANAGE, Some("ALL"), Some(own_id)),
                other_id,
            ),
            None,
            "employee management must not substitute for private employee reads"
        );
        assert_eq!(
            employee_target_access_from_claims(
                &claims(PERM_EMPLOYEE_READ, Some("SELF"), Some(own_id)),
                own_id,
            ),
            Some(EmployeeTargetAccess::SelfBound)
        );
        assert_eq!(
            employee_target_access_from_claims(
                &claims(PERM_EMPLOYEE_READ, Some("TEAM"), Some(own_id)),
                other_id,
            ),
            Some(EmployeeTargetAccess::ScopedRead)
        );
        assert_eq!(
            employee_target_access_from_claims(
                &claims(PERM_EMPLOYEE_READ, Some("ALL"), Some(own_id)),
                other_id,
            ),
            Some(EmployeeTargetAccess::ScopedRead)
        );
    }

    #[test]
    fn payroll_sensitive_access_requires_permission_and_scope() {
        let own_id = Uuid::new_v4();
        let mut scope_only = claims(PERM_EMPLOYEE_READ, Some("SELF"), Some(own_id));
        scope_only
            .permission_scopes
            .insert(PERM_PAYROLL_READ.into(), "ALL".into());

        assert!(!can_view_payroll_sensitive_from_claims(&scope_only, own_id));
        assert!(can_view_payroll_sensitive_from_claims(
            &claims(PERM_PAYROLL_READ, Some("SELF"), Some(own_id)),
            own_id
        ));
        assert!(can_view_payroll_sensitive_from_claims(
            &claims(PERM_PAYROLL_READ, Some("ALL"), None),
            own_id
        ));
    }

    #[tokio::test]
    async fn every_protected_employee_query_requires_its_exact_permission_before_db_access() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let record_id = Uuid::new_v4();
        let fields = vec![
            (
                "{ employeeDirectoryPage { __typename } }".to_string(),
                PERM_EMPLOYEE_DIRECTORY_READ,
                true,
            ),
            (
                "{ organizationDirectoryChart { __typename } }".to_string(),
                PERM_EMPLOYEE_DIRECTORY_READ,
                true,
            ),
            (
                format!("{{ employeeProfileAccess(employeeId: \"{other_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_DIRECTORY_READ,
                true,
            ),
            (
                format!("{{ employeeProfileChangeRequests(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ employeeProfileReviewQueue { __typename } }".to_string(),
                PERM_EMPLOYEE_MANAGE,
                true,
            ),
            (
                format!("{{ employeeProfileChangeReviewDetail(requestId: \"{record_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_MANAGE,
                true,
            ),
            (
                "{ employeeEvidenceReviewQueue { __typename } }".to_string(),
                PERM_EMPLOYEE_MANAGE,
                true,
            ),
            (
                format!("{{ employeeEducationRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employeeWorkExperienceRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employee(id: \"{other_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!(
                    "{{ _entities(representations: [{{ __typename: \"Employee\", id: \"{other_id}\" }}]) {{ ... on Employee {{ id }} }} }}"
                ),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ myEmployee { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ employees { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employmentHistoryRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_PAYROLL_READ,
                false,
            ),
            (
                "{ documentTypes { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ companyDocuments { __typename } }".to_string(),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                "{ employeeDocuments { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employeePrimaryBank(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employeeIdentityProfile(employeeId: \"{own_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ departments { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ designations { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ expenseAssignableRoles { __typename } }".to_string(),
                PERM_EXPENSE_MANAGE,
                true,
            ),
            (
                "{ orgChart { __typename } }".to_string(),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ employeeDocumentAttachment(employeeDocumentId: \"{record_id}\") {{ __typename }} }}"),
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ companyDocumentAttachment(companyDocumentId: \"{record_id}\") {{ __typename }} }}"),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                "{ onboardingChecklist { __typename } }".to_string(),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                "{ separations { __typename } }".to_string(),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                format!("{{ fnfSettlement(separationId: \"{record_id}\") {{ __typename }} }}"),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                format!("{{ clearanceChecklist(separationId: \"{record_id}\") {{ __typename }} }}"),
                PERM_ONBOARDING_SELF,
                false,
            ),
            (
                "{ tenantDirectoryUsers { __typename } }".to_string(),
                PERM_ROLE_MANAGE,
                true,
            ),
            (
                "{ tenantDirectoryRoles { __typename } }".to_string(),
                PERM_ROLE_MANAGE,
                true,
            ),
            (
                "{ tenantCatalogPermissions { __typename } }".to_string(),
                PERM_ROLE_MANAGE,
                true,
            ),
            (
                format!("{{ permissionIdsForRole(roleId: \"{record_id}\") }}"),
                PERM_ROLE_MANAGE,
                true,
            ),
            (
                format!("{{ roleIdsForUser(userId: \"{record_id}\") }}"),
                PERM_ROLE_MANAGE,
                true,
            ),
            (
                format!("{{ permissionScopesForRole(roleId: \"{record_id}\") {{ __typename }} }}"),
                PERM_ROLE_MANAGE,
                true,
            ),
        ];

        for (query, required_permission, requires_all) in fields {
            for denied_claims in [
                claims("unrelated:read", Some("ALL"), Some(own_id)),
                claims(
                    sibling_permission(required_permission),
                    Some("ALL"),
                    Some(own_id),
                ),
            ] {
                let response = execute_query(denied_claims, &query).await;
                assert_forbidden_before_db(&response, required_permission);
            }

            for scope in [None, Some("INVALID")] {
                let response = execute_query(
                    claims(required_permission, scope, Some(own_id)),
                    &query,
                )
                .await;
                assert_forbidden_before_db(&response, required_permission);
            }

            if requires_all {
                for scope in ["SELF", "TEAM", "DEPARTMENT"] {
                    let response = execute_query(
                        claims(required_permission, Some(scope), Some(own_id)),
                        &query,
                    )
                    .await;
                    assert_forbidden_before_db(&response, required_permission);
                }
            }
        }
    }

    #[tokio::test]
    async fn directory_employee_and_payroll_permissions_do_not_substitute_each_other() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let employee_record_queries = [
            format!("{{ employeeProfileChangeRequests(employeeId: \"{own_id}\") {{ __typename }} }}"),
            format!("{{ employeeEducationRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
            format!("{{ employeeWorkExperienceRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
            format!("{{ employeePrimaryBank(employeeId: \"{own_id}\") {{ __typename }} }}"),
            format!("{{ employeeIdentityProfile(employeeId: \"{own_id}\") {{ __typename }} }}"),
        ];
        for query in employee_record_queries {
            let response = execute_query(
                claims(PERM_EMPLOYEE_DIRECTORY_READ, Some("ALL"), Some(own_id)),
                &query,
            )
            .await;
            assert_forbidden_before_db(&response, PERM_EMPLOYEE_READ);
        }

        let salary_response = execute_query(
            claims(PERM_EMPLOYEE_READ, Some("ALL"), Some(own_id)),
            &format!("{{ employmentHistoryRecords(employeeId: \"{own_id}\") {{ __typename }} }}"),
        )
        .await;
        assert_forbidden_before_db(&salary_response, PERM_PAYROLL_READ);

        let directory_response = execute_query(
            claims(PERM_EMPLOYEE_READ, Some("ALL"), Some(own_id)),
            &format!("{{ employeeProfileAccess(employeeId: \"{other_id}\") {{ __typename }} }}"),
        )
        .await;
        assert_forbidden_before_db(&directory_response, PERM_EMPLOYEE_DIRECTORY_READ);
    }

    #[tokio::test]
    async fn salary_history_accepts_only_payroll_self_or_payroll_all() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let own_query = format!(
            "{{ employmentHistoryRecords(employeeId: \"{own_id}\") {{ __typename }} }}"
        );
        let other_query = format!(
            "{{ employmentHistoryRecords(employeeId: \"{other_id}\") {{ __typename }} }}"
        );

        assert_authorized_before_db(
            &execute_query(
                claims(PERM_PAYROLL_READ, Some("SELF"), Some(own_id)),
                &own_query,
            )
            .await,
        );
        assert_authorized_before_db(
            &execute_query(
                claims(PERM_PAYROLL_READ, Some("ALL"), Some(own_id)),
                &other_query,
            )
            .await,
        );

        let manager_response = execute_query(
            claims(PERM_EMPLOYEE_READ, Some("TEAM"), Some(own_id)),
            &other_query,
        )
        .await;
        assert_forbidden_before_db(&manager_response, PERM_PAYROLL_READ);

        let employee_admin_response = execute_query(
            claims(PERM_EMPLOYEE_MANAGE, Some("ALL"), Some(own_id)),
            &other_query,
        )
        .await;
        assert_forbidden_before_db(&employee_admin_response, PERM_PAYROLL_READ);
    }

    #[tokio::test]
    async fn salary_history_does_not_reuse_malformed_employee_read_grants() {
        let own_id = Uuid::new_v4();
        let own_query = format!(
            "{{ employmentHistoryRecords(employeeId: \"{own_id}\") {{ __typename }} }}"
        );

        let payroll_all = claims(PERM_PAYROLL_READ, Some("ALL"), Some(own_id));

        let mut malformed_employee_read_with_payroll_all =
            claims(PERM_EMPLOYEE_READ, Some("INVALID"), Some(own_id));
        malformed_employee_read_with_payroll_all
            .permissions
            .push(PERM_PAYROLL_READ.into());
        malformed_employee_read_with_payroll_all
            .permission_scopes
            .insert(PERM_PAYROLL_READ.into(), "ALL".into());

        for (case, claims) in [
            ("payroll read ALL", payroll_all),
            (
                "malformed employee read plus payroll read ALL",
                malformed_employee_read_with_payroll_all,
            ),
        ] {
            let response = execute_query(claims, &own_query).await;
            assert_eq!(
                response.errors.len(),
                1,
                "unexpected response for {case}: {response:?}"
            );
            assert_eq!(
                response.errors[0].message, "internal server error",
                "valid independent grant was rejected for {case}"
            );
        }
    }

    #[tokio::test]
    async fn salary_history_rejects_missing_malformed_and_bounded_admin_scopes() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let own_query = format!(
            "{{ employmentHistoryRecords(employeeId: \"{own_id}\") {{ __typename }} }}"
        );
        let other_query = format!(
            "{{ employmentHistoryRecords(employeeId: \"{other_id}\") {{ __typename }} }}"
        );

        for scope in [None, Some("INVALID"), Some("TEAM"), Some("DEPARTMENT")] {
            let response = execute_query(
                claims(PERM_PAYROLL_READ, scope, Some(own_id)),
                &own_query,
            )
            .await;
            assert_forbidden_before_db(&response, PERM_PAYROLL_READ);
        }

        for scope in [None, Some("INVALID"), Some("SELF"), Some("TEAM"), Some("DEPARTMENT")] {
            let response = execute_query(
                claims(PERM_PAYROLL_READ, scope, Some(own_id)),
                &other_query,
            )
            .await;
            assert_forbidden_before_db(&response, PERM_PAYROLL_READ);
        }
    }

    #[tokio::test]
    async fn role_onboarding_and_expense_admin_queries_remain_permission_separated() {
        let record_id = Uuid::new_v4();
        let cases = [
            (
                "{ tenantDirectoryRoles { __typename } }".to_string(),
                PERM_EMPLOYEE_MANAGE,
                PERM_ROLE_MANAGE,
            ),
            (
                "{ expenseAssignableRoles { __typename } }".to_string(),
                PERM_ROLE_MANAGE,
                PERM_EXPENSE_MANAGE,
            ),
            (
                "{ separations { __typename } }".to_string(),
                PERM_EMPLOYEE_MANAGE,
                PERM_ONBOARDING_SELF,
            ),
            (
                format!("{{ companyDocumentAttachment(companyDocumentId: \"{record_id}\") {{ __typename }} }}"),
                PERM_ROLE_MANAGE,
                PERM_ONBOARDING_SELF,
            ),
        ];

        for (query, supplied, required) in cases {
            let response = execute_query(claims(supplied, Some("ALL"), None), &query).await;
            assert_forbidden_before_db(&response, required);
        }
    }

    #[tokio::test]
    async fn onboarding_self_and_manage_are_target_and_scope_specific() {
        let own_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let separation_id = Uuid::new_v4();

        assert_authorized_before_db(
            &execute_query(
                claims(PERM_ONBOARDING_SELF, Some("SELF"), Some(own_id)),
                "{ onboardingChecklist { __typename } }",
            )
            .await,
        );
        assert_forbidden_before_db(
            &execute_query(
                claims(PERM_ONBOARDING_SELF, Some("SELF"), Some(own_id)),
                &format!(
                    "{{ onboardingChecklist(employeeId: \"{other_id}\") {{ __typename }} }}"
                ),
            )
            .await,
            PERM_ONBOARDING_MANAGE,
        );
        assert_authorized_before_db(
            &execute_query(
                claims(PERM_ONBOARDING_MANAGE, Some("ALL"), Some(own_id)),
                &format!(
                    "{{ onboardingChecklist(employeeId: \"{other_id}\") {{ __typename }} }}"
                ),
            )
            .await,
        );

        for scope in [None, Some("INVALID"), Some("SELF"), Some("TEAM"), Some("DEPARTMENT")] {
            let response = execute_query(
                claims(PERM_ONBOARDING_MANAGE, scope, Some(own_id)),
                &format!(
                    "{{ companyDocumentAttachment(companyDocumentId: \"{separation_id}\") {{ __typename }} }}"
                ),
            )
            .await;
            assert_forbidden_before_db(&response, PERM_ONBOARDING_MANAGE);
        }
    }

    #[tokio::test]
    async fn scoped_target_loader_uses_shared_scope_resolver_and_short_circuits_outside_targets() {
        let target_id = Uuid::new_v4();
        let (disconnected, _) = proxy_database(Vec::new()).await;
        let loaded = load_scoped_employee_target_with_connection(
            &disconnected,
            Uuid::new_v4(),
            target_id,
            ScopeType::All,
            None,
            |employee_id| async move { Ok(Some(employee_id)) },
        )
        .await
        .expect("ALL target load");
        assert_eq!(loaded, Some(target_id));

        let viewer_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let (team_db, statements) =
            proxy_database(vec![id_rows(&[viewer_id, direct_report_id])]).await;
        let load_called = Cell::new(false);
        let load_called_ref = &load_called;
        let hidden = load_scoped_employee_target_with_connection(
            &team_db,
            Uuid::new_v4(),
            target_id,
            ScopeType::Team,
            Some(ClientViewerEmployee {
                employee_id: viewer_id,
                department_id: None,
            }),
            |employee_id| async move {
                load_called_ref.set(true);
                Ok(Some(employee_id))
            },
        )
        .await
        .expect("out-of-scope targets are hidden");
        assert_eq!(hidden, None);
        assert!(!load_called.get(), "out-of-scope targets must not be loaded");
        assert_eq!(statements.lock().expect("TEAM statements").len(), 1);
    }

    #[tokio::test]
    async fn scoped_target_loader_executes_recursive_team_sql_with_tenant_and_manager_bindings() {
        let manager_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let recursive_report_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let (db, statements) = proxy_database(vec![id_rows(&[
            manager_id,
            direct_report_id,
            recursive_report_id,
        ])])
        .await;

        let loaded = load_scoped_employee_target_with_connection(
            &db,
            tenant_id,
            recursive_report_id,
            ScopeType::Team,
            Some(ClientViewerEmployee {
                employee_id: manager_id,
                department_id: None,
            }),
            |employee_id| async move { Ok(Some(employee_id)) },
        )
        .await
        .expect("recursive TEAM target load");

        assert_eq!(loaded, Some(recursive_report_id));
        let statements = statements.lock().expect("recursive TEAM statements");
        assert_eq!(statements.len(), 1);
        let statement = &statements[0];
        assert_eq!(statement.db_backend, DbBackend::Postgres);
        assert!(statement.sql.contains("WITH RECURSIVE team"));
        assert!(statement.sql.contains("root.tenant_id = $1"));
        assert!(statement.sql.contains("child.tenant_id = $1"));
        assert_eq!(
            statement.values.as_ref().expect("TEAM bound values").0,
            vec![
                tenant_id.into(),
                manager_id.into(),
                EMPLOYMENT_STATUS_ACTIVE.into(),
                EMPLOYMENT_STATUS_PROBATION.into(),
            ]
        );
    }

    #[tokio::test]
    async fn scoped_target_loader_denies_self_and_team_without_a_viewer_without_querying() {
        for scope in [ScopeType::Self_, ScopeType::Team] {
            let (db, statements) = proxy_database(Vec::new()).await;
            let load_called = Cell::new(false);
            let load_called_ref = &load_called;
            let result = load_scoped_employee_target_with_connection(
                &db,
                Uuid::new_v4(),
                Uuid::new_v4(),
                scope,
                None,
                |employee_id| async move {
                    load_called_ref.set(true);
                    Ok(Some(employee_id))
                },
            )
            .await
            .expect("viewerless bounded scope is uniformly hidden");

            assert_eq!(result, None);
            assert!(!load_called.get());
            assert!(statements.lock().expect("viewerless statements").is_empty());
        }
    }

    #[tokio::test]
    async fn employee_attachment_parent_lookup_is_tenant_and_employee_scope_bound() {
        let document_id = Uuid::new_v4();
        let employee_id = Uuid::new_v4();
        let now = Utc::now();
        let document = employee_document::Model {
            id: document_id,
            tenant_id: Uuid::new_v4(),
            employee_id,
            document_type_id: Uuid::new_v4(),
            file_storage_id: Some(Uuid::new_v4()),
            status: "PENDING".into(),
            expiry_date: None,
            workflow_instance_id: None,
            uploaded_at: now,
            verified_by: None,
            verified_at: None,
            is_deleted: false,
            deleted_at: None,
            deleted_by: None,
            created_at: now,
            updated_at: now,
        };
        let (allowed_db, allowed_statements) =
            proxy_database(vec![vec![employee_document_row(&document)]]).await;
        let allowed = document_service::find_employee_document_in_scope(
            &allowed_db,
            document.tenant_id,
            document_id,
            &EmployeeScopeFilter::EmployeeIds(vec![employee_id]),
        )
        .await
        .expect("authorized employee document lookup");
        assert_eq!(allowed, Some(document.clone()));
        let allowed_statements = allowed_statements.lock().expect("document statements");
        let allowed_sql = normalized_sql(&allowed_statements[0]);
        assert!(allowed_sql.contains("employee_document.tenant_id"));
        assert!(allowed_sql.contains("employee_document.employee_id"));

        let outside_employee_id = Uuid::new_v4();
        let (denied_db, _) = proxy_database(vec![Vec::new()]).await;
        let denied = document_service::find_employee_document_in_scope(
            &denied_db,
            document.tenant_id,
            document_id,
            &EmployeeScopeFilter::EmployeeIds(vec![outside_employee_id]),
        )
        .await
        .expect("out-of-scope document is hidden");
        let (missing_db, _) = proxy_database(vec![Vec::new()]).await;
        let missing = document_service::find_employee_document_in_scope(
            &missing_db,
            document.tenant_id,
            Uuid::new_v4(),
            &EmployeeScopeFilter::EmployeeIds(vec![employee_id]),
        )
        .await
        .expect("missing document is hidden");

        assert_eq!(denied, missing);
        assert_eq!(denied, None);
    }

    #[tokio::test]
    async fn company_attachment_parent_lookup_hides_nonvisible_documents_uniformly() {
        use crate::entities::d0056_company_documents::company_document;

        let now = Utc::now();
        let document = company_document::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            category: "POLICY".into(),
            title: "Policy".into(),
            description: None,
            file_storage_id: Uuid::new_v4(),
            status: "ACTIVE".into(),
            visible_to_employees: true,
            uploaded_by: None,
            is_deleted: false,
            deleted_at: None,
            deleted_by: None,
            created_at: now,
            updated_at: now,
        };
        let (self_db, self_statements) =
            proxy_database(vec![vec![company_document_row(&document)]]).await;
        let visible = company_document_service::find_visible_company_document(
            &self_db,
            document.tenant_id,
            document.id,
            false,
        )
        .await
        .expect("employee-visible company document lookup");
        assert_eq!(visible, Some(document.clone()));
        let self_statements = self_statements.lock().expect("company statements");
        let self_sql = normalized_sql(&self_statements[0]);
        assert!(self_sql.contains("company_document.visible_to_employees"));
        assert!(self_sql.contains("company_document.status"));
        assert_eq!(
            self_statements[0]
                .values
                .as_ref()
                .expect("employee-visible company document bound values")
                .0,
            vec![
                document.id.into(),
                document.tenant_id.into(),
                false.into(),
                true.into(),
                "ACTIVE".into(),
                1_u64.into(),
            ]
        );

        let mut hidden_document = document.clone();
        hidden_document.status = "DRAFT".into();
        hidden_document.visible_to_employees = false;
        let (manage_db, manage_statements) =
            proxy_database(vec![vec![company_document_row(&hidden_document)]]).await;
        let managed = company_document_service::find_visible_company_document(
            &manage_db,
            hidden_document.tenant_id,
            hidden_document.id,
            true,
        )
        .await
        .expect("manager-visible company document lookup");
        assert_eq!(managed, Some(hidden_document));
        let manage_statements = manage_statements.lock().expect("managed company statements");
        assert_eq!(
            manage_statements[0]
                .values
                .as_ref()
                .expect("managed company document bound values")
                .0,
            vec![
                document.id.into(),
                document.tenant_id.into(),
                false.into(),
                1_u64.into()
            ]
        );

        let (denied_db, _) = proxy_database(vec![Vec::new()]).await;
        let denied = company_document_service::find_visible_company_document(
            &denied_db,
            document.tenant_id,
            document.id,
            false,
        )
        .await
        .expect("hidden document is uniformly absent");
        let (missing_db, _) = proxy_database(vec![Vec::new()]).await;
        let missing = company_document_service::find_visible_company_document(
            &missing_db,
            document.tenant_id,
            Uuid::new_v4(),
            false,
        )
        .await
        .expect("missing document is uniformly absent");
        assert_eq!(denied, missing);
        assert_eq!(denied, None);
    }

    #[tokio::test]
    async fn offboarding_parent_lookup_returns_uniform_contracts_before_child_reads() {
        use crate::entities::d0017_onboarding_offboarding::{
            clearance_checklist, fnf_settlement, separation,
        };

        let now = Utc::now();
        let employee_id = Uuid::new_v4();
        let separation = separation::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            employee_id,
            separation_type: "RESIGNATION".into(),
            resignation_date: None,
            last_working_date: NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid date"),
            reason: None,
            status: "APPROVED".into(),
            approved_by: None,
            workflow_instance_id: None,
            offboarded_at: None,
            offboarding_event_id: None,
            created_at: now,
            updated_at: now,
        };
        let fnf = fnf_settlement::Model {
            id: Uuid::new_v4(),
            tenant_id: separation.tenant_id,
            separation_id: separation.id,
            leave_encashment: None,
            gratuity_amount: None,
            bonus_payable: None,
            recovery_amount: None,
            net_payable: None,
            status: "DRAFT".into(),
            processed_at: None,
            processed_by: None,
            created_at: now,
            updated_at: now,
        };
        let clearance = clearance_checklist::Model {
            id: Uuid::new_v4(),
            tenant_id: separation.tenant_id,
            separation_id: separation.id,
            department: "IT".into(),
            task_name: "Return equipment".into(),
            is_cleared: false,
            cleared_by: None,
            cleared_at: None,
            created_at: now,
            updated_at: now,
        };

        let (fnf_db, fnf_statements) = proxy_database(vec![
            vec![separation_row(&separation)],
            vec![fnf_row(&fnf)],
        ])
        .await;
        let visible_fnf = offboarding_fnf_service::get_visible_fnf_by_separation(
            &fnf_db,
            separation.tenant_id,
            separation.id,
            Some(employee_id),
        )
        .await
        .expect("self-visible FNF lookup");
        assert_eq!(visible_fnf, Some(fnf));
        let fnf_statements = fnf_statements.lock().expect("FNF statements");
        assert_eq!(fnf_statements.len(), 2);
        assert_eq!(
            fnf_statements[0]
                .values
                .as_ref()
                .expect("self-visible separation bound values")
                .0,
            vec![
                separation.id.into(),
                separation.tenant_id.into(),
                employee_id.into(),
                1_u64.into()
            ]
        );

        let (clearance_db, clearance_statements) = proxy_database(vec![
            vec![separation_row(&separation)],
            vec![clearance_row(&clearance)],
        ])
        .await;
        let visible_clearance = offboarding_fnf_service::list_visible_clearance(
            &clearance_db,
            separation.tenant_id,
            separation.id,
            None,
        )
        .await
        .expect("manager-visible clearance lookup");
        assert_eq!(visible_clearance, vec![clearance]);
        let clearance_statements = clearance_statements.lock().expect("clearance statements");
        assert_eq!(clearance_statements.len(), 2);
        assert_eq!(
            clearance_statements[0]
                .values
                .as_ref()
                .expect("manager-visible separation bound values")
                .0,
            vec![
                separation.id.into(),
                separation.tenant_id.into(),
                1_u64.into()
            ]
        );

        let (denied_fnf_db, denied_fnf_statements) = proxy_database(vec![Vec::new()]).await;
        let denied_fnf = offboarding_fnf_service::get_visible_fnf_by_separation(
            &denied_fnf_db,
            separation.tenant_id,
            separation.id,
            Some(Uuid::new_v4()),
        )
        .await
        .expect("out-of-scope FNF is absent");
        let (missing_fnf_db, _) = proxy_database(vec![Vec::new()]).await;
        let missing_fnf = offboarding_fnf_service::get_visible_fnf_by_separation(
            &missing_fnf_db,
            separation.tenant_id,
            Uuid::new_v4(),
            Some(employee_id),
        )
        .await
        .expect("missing FNF is absent");
        assert_eq!(denied_fnf, missing_fnf);
        assert_eq!(denied_fnf, None);
        assert_eq!(denied_fnf_statements.lock().expect("denied FNF statements").len(), 1);

        let (denied_clearance_db, denied_clearance_statements) =
            proxy_database(vec![Vec::new()]).await;
        let denied_clearance = offboarding_fnf_service::list_visible_clearance(
            &denied_clearance_db,
            separation.tenant_id,
            separation.id,
            Some(Uuid::new_v4()),
        )
        .await
        .expect("out-of-scope clearance is empty");
        let (missing_clearance_db, _) = proxy_database(vec![Vec::new()]).await;
        let missing_clearance = offboarding_fnf_service::list_visible_clearance(
            &missing_clearance_db,
            separation.tenant_id,
            Uuid::new_v4(),
            Some(employee_id),
        )
        .await
        .expect("missing clearance is empty");
        assert_eq!(denied_clearance, missing_clearance);
        assert!(denied_clearance.is_empty());
        assert_eq!(
            denied_clearance_statements
                .lock()
                .expect("denied clearance statements")
                .len(),
            1
        );

    }
}
