//! Root query resolvers for kabipay-employee.

use async_graphql::{Context, Object, Result, ID};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use kabipay_common::{
    subgraph::{
        require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db,
    },
    KabiPayError,
};
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
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::entities::d0007_employee_core::employee;
use crate::entities::d0008_document_system::employee_document;
use crate::entities::d0029_file_storage::file_storage;
use crate::resolvers::scope::{
    assert_employee_in_data_scope, data_scope_employee, require_tenant_rbac_admin,
    resolve_viewer_employee,
};
use crate::services::{company_document_service, document_file_service};
use crate::services::{
    directory_service, document_service, employee_service, employment_history_service,
    offboarding_fnf_service, onboarding_service, org_service, profile_change_service,
    profile_extras_service, profile_record_service, rbac_admin_service, separation_service,
};

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Liveness probe for this federated subgraph. Always returns `ok`.
    async fn employee_health(&self) -> &'static str {
        "ok"
    }

    /// Safe company directory available to every authenticated tenant employee.
    async fn employee_directory_page(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        after: Option<String>,
    ) -> Result<EmployeeDirectoryPageDto> {
        let _claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let page = directory_service::list_page(&db, tenant_id, limit, after.as_deref())
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = enrich_directory_entries(&db, tenant_id, page.rows).await?;
        Ok(EmployeeDirectoryPageDto {
            has_more: page.next_cursor.is_some(),
            next_cursor: page.next_cursor,
            rows,
        })
    }

    /// Safe full reporting hierarchy for authenticated employees.
    async fn organization_directory_chart(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<EmployeeDirectoryEntryDto>> {
        let _claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = directory_service::list_hierarchy(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        enrich_directory_entries(&db, tenant_id, rows).await
    }

    /// Public profile projection plus server-derived private/edit capabilities.
    async fn employee_profile_access(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Option<EmployeeProfileAccessDto>> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let target_id = parse_uuid(&employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let Some(target) = directory_service::find_current_by_id(&db, tenant_id, target_id)
            .await
            .map_err(KabiPayError::into_graphql)?
        else {
            return Ok(None);
        };
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        let is_self = viewer_id == Some(target_id);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let in_scope = employee_service::is_employee_in_scope(data_scope_employee(ctx), viewer, &target);
        let can_manage = claims.can_manage_employee_directory() && in_scope;
        let mut entries = enrich_directory_entries(&db, tenant_id, vec![target]).await?;
        let directory_entry = entries.pop().ok_or_else(|| {
            KabiPayError::Internal("profile directory entry missing".into()).into_graphql()
        })?;
        Ok(Some(EmployeeProfileAccessDto {
            directory_entry,
            is_self,
            can_view_private_profile: is_self || can_manage,
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
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let target_id = parse_uuid(&employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(target_id) {
            if !claims.can_manage_employee_directory() {
                return Err(KabiPayError::Forbidden(
                    "profile change requests are private to the employee and HR".into(),
                )
                .into_graphql());
            }
            assert_employee_in_data_scope(ctx, &db, tenant_id, target_id).await?;
        }
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden("employee:write is required for the profile review queue".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let scope = data_scope_employee(ctx);
        let employee_ids = employee_service::employee_ids_in_scope(&db, tenant_id, scope, viewer)
            .await.map_err(KabiPayError::into_graphql)?;
        let rows = profile_change_service::list_review_queue(
            &db,
            tenant_id,
            status.as_deref().or(Some("PENDING")),
            (limit.clamp(1, 100) as u64).saturating_mul(4),
            employee_ids.as_deref(),
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
            if !employee_service::is_employee_in_scope(scope, viewer, employee) {
                continue;
            }
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden("employee:write is required to inspect profile changes".into()).into_graphql());
        }
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
        assert_employee_in_data_scope(ctx, &db, tenant_id, request.employee_id).await?;
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden("employee:write is required for evidence review".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_employee(ctx);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let employee_ids = employee_service::employee_ids_in_scope(&db, tenant_id, scope, viewer)
            .await.map_err(KabiPayError::into_graphql)?;
        let records = profile_record_service::list_pending_evidence_reviews(&db, tenant_id, limit.clamp(1, 100) as u64, employee_ids.as_deref())
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
            if !employee_service::is_employee_in_scope(scope, viewer, employee) { continue; }
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
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) {
            if !claims.can_manage_employee_directory() {
                return Err(KabiPayError::Forbidden(
                    "education records are private to the employee and HR".into(),
                )
                .into_graphql());
            }
            assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        }
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
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) {
            if !claims.can_manage_employee_directory() {
                return Err(KabiPayError::Forbidden(
                    "work experience records are private to the employee and HR".into(),
                )
                .into_graphql());
            }
            assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        }
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_employee(ctx);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let models = employee_service::list(&db, tenant_id, limit, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let dtos: Vec<EmployeeDto> = models.into_iter().map(EmployeeDto::from).collect();
        enrich_employee_dtos(&db, tenant_id, dtos).await
    }

    /// Compensation rows driving payroll base salary (`employment_history`), newest first.
    /// Allowed for **`employee:write`**, **`payroll:statutory_export`**, or the employee themself.
    async fn employment_history_records(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        #[graphql(default = 24)] limit: u64,
    ) -> Result<Vec<EmploymentHistoryRecordDto>> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let viewer_eid = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        let is_self = viewer_eid == Some(eid);
        if !claims.can_manage_employee_directory()
            && !claims.can_export_payroll_statutory()
            && !is_self
        {
            return Err(
                KabiPayError::Forbidden(
                    "employment history requires employee:write, payroll export permission, or your own employee id"
                        .into(),
                )
                .into_graphql(),
            );
        }
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
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let include_hidden = claims.can_manage_employee_directory()
            || claims.can_manage_onboarding_tenant()
            || claims.can_manage_tenant_rbac();
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(id) = &employee_id {
            let eid = parse_uuid(id, "employee id")?;
            assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
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
    ) -> Result<Vec<DesignationDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = org_service::list_designations(&db, tenant_id, limit)
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_expense_configuration() {
            return Err(
                KabiPayError::Forbidden(
                    "expense configuration permission required to list roles for policies".into(),
                )
                .into_graphql(),
            );
        }
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_employee(ctx);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let doc_id = parse_uuid(&employee_document_id, "employeeDocumentId")?;
        let model = employee_document::Entity::find_by_id(doc_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
            .filter(employee_document::Column::IsDeleted.eq(false))
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
        let file_id = model.file_storage_id.ok_or_else(|| {
            KabiPayError::Validation("document has no file yet".to_string()).into_graphql()
        })?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, model.employee_id).await?;
        let fs_row = file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "fileStorage",
                    id: file_id.to_string(),
                }
                .into_graphql()
            })?;
        let bytes = document_file_service::read_stored_file_bytes(
            &document_file_service::local_file_root(),
            &fs_row,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
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

    /// Private tenant file bytes for generic `file_storage` uploads.
    ///
    /// This is intentionally not a public/signed URL. Generic file storage is shared by multiple
    /// HRMS modules, so reads are limited to the uploader or elevated tenant HR/onboarding/RBAC
    /// admins until each module has its own business-object-specific visibility rules.
    async fn tenant_file_attachment(
        &self,
        ctx: &Context<'_>,
        file_storage_id: ID,
    ) -> Result<TenantFileAttachmentDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let file_id = parse_uuid(&file_storage_id, "fileStorageId")?;
        let fs_row = file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "fileStorage",
                    id: file_id.to_string(),
                }
                .into_graphql()
            })?;

        let uploaded_by_viewer = fs_row.uploaded_by == Some(claims.sub);
        let can_read_tenant_admin_file = claims.can_manage_employee_directory()
            || claims.can_manage_onboarding_tenant()
            || claims.can_manage_tenant_rbac();
        if !uploaded_by_viewer && !can_read_tenant_admin_file {
            return Err(KabiPayError::Forbidden(
                "file is private to the uploader or tenant HR/onboarding admins".to_string(),
            )
            .into_graphql());
        }

        let bytes = document_file_service::read_stored_file_bytes(
            &document_file_service::local_file_root(),
            &fs_row,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TenantFileAttachmentDto {
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
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let document_id = parse_uuid(&company_document_id, "companyDocumentId")?;
        let doc = company_document_service::find_company_document(&db, tenant_id, document_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let can_manage = claims.can_manage_employee_directory()
            || claims.can_manage_onboarding_tenant()
            || claims.can_manage_tenant_rbac();
        if !can_manage && (doc.status != "ACTIVE" || !doc.visible_to_employees) {
            return Err(KabiPayError::Forbidden(
                "company document is not visible to employees".to_string(),
            )
            .into_graphql());
        }
        let fs_row = file_storage::Entity::find_by_id(doc.file_storage_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "fileStorage",
                    id: doc.file_storage_id.to_string(),
                }
                .into_graphql()
            })?;
        let bytes = document_file_service::read_stored_file_bytes(
            &document_file_service::local_file_root(),
            &fs_row,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(id) = &employee_id {
            let eid = parse_uuid(id, "employee id")?;
            assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let claims = require_client_claims(ctx)?;
        let filter = if claims.can_manage_onboarding_tenant() {
            None
        } else if claims.can_use_onboarding_self_service() {
            Some(
                resolve_client_employee_id(ctx, &db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)?,
            )
        } else {
            return Err(
                KabiPayError::Forbidden("onboarding:self or onboarding:manage permission required".into())
                    .into_graphql(),
            );
        };
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
        let tenant_id = require_tenant_id(ctx)?;
        let sid = parse_uuid(&separation_id, "separation id")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sep = offboarding_fnf_service::get_separation_tenant(&db, tenant_id, sid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some(sep) = sep else {
            return Ok(None);
        };
        let claims = require_client_claims(ctx)?;
        let tenant_wide_fnf = claims.can_manage_onboarding_tenant()
            || claims.can_manage_employee_directory();
        if !tenant_wide_fnf {
            let viewer = resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?;
            if sep.employee_id != viewer {
                return Err(
                    KabiPayError::Forbidden("cannot view FNF for this separation".into())
                        .into_graphql(),
                );
            }
        }
        let m = offboarding_fnf_service::get_fnf_by_separation(&db, tenant_id, sid)
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
        let tenant_id = require_tenant_id(ctx)?;
        let sid = parse_uuid(&separation_id, "separation id")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sep = offboarding_fnf_service::get_separation_tenant(&db, tenant_id, sid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some(sep) = sep else {
            return Ok(vec![]);
        };
        let claims = require_client_claims(ctx)?;
        let tenant_wide_clearance = claims.can_manage_onboarding_tenant()
            || claims.can_manage_employee_directory();
        if !tenant_wide_clearance {
            let viewer = resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?;
            if sep.employee_id != viewer {
                return Err(
                    KabiPayError::Forbidden("cannot view clearance for this separation".into())
                        .into_graphql(),
                );
            }
        }
        let rows = offboarding_fnf_service::list_clearance(&db, tenant_id, sid)
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
    let tenant_id = require_tenant_id(ctx)?;
    let employee_id = parse_uuid(&id, "employee id")?;
    let db = tenant_db(ctx, tenant_id).await?;
    let model = employee_service::find_by_id(&db, tenant_id, employee_id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let model = if let Some(ref m) = model {
        let scope = data_scope_employee(ctx);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        if employee_service::is_employee_in_scope(scope, viewer, m) {
            model
        } else {
            None
        }
    } else {
        model
    };
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
