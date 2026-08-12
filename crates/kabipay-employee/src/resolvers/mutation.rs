//! GraphQL mutations for employees.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::ClientClaims,
    password,
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::query::enrich_employee_dtos;
use crate::resolvers::scope::{assert_employee_in_data_scope, require_tenant_rbac_admin};
use crate::resolvers::types::{
    ClearanceChecklistItemDto, CreateEmployeeInput, EmployeeAadhaarRecordDto,
    EmployeeBankAccountDto, EmployeeDocumentDto, EmployeeDto, EmployeePanRecordDto,
    EmployeeEducationDto, EmployeeProfileChangeRequestDto, EmployeeWorkExperienceDto,
    EmploymentHistoryRecordDto, FnfSettlementDto,
    OnboardingChecklistItemDto,
    PermissionScopeAssignmentInput, ProvisionEmployeeLoginInput, ResetEmployeePasswordInput,
    SeparationDto, SetEmployeeCompensationInput, SubmitEmployeeProfileChangeInput,
    SubmitSeparationInput, UpdateEmployeeInput, UpdateEmployeePersonalProfileInput,
    UpdateEmployeeSelfServiceProfileInput, UploadEmployeeDocumentInput, UploadTenantFileInput,
    UploadedTenantFileDto, UpsertEmployeePrimaryAadhaarInput, UpsertEmployeePrimaryBankInput,
    UpsertEmployeeEducationInput, UpsertEmployeePrimaryPanInput,
    UpsertEmployeeWorkExperienceInput, UpsertFnfSettlementInput,
};
use crate::services::document_file_service;
use crate::services::document_service;
use crate::services::employee_service::{
    self, EmployeePatch, NewEmployee, PersonalProfilePatch, SelfServiceProfilePatch,
};
use crate::services::employment_history_service;
use crate::services::offboarding_fnf_service;
use crate::services::onboarding_service;
use crate::services::profile_extras_service;
use crate::services::profile_change_service::{self, NewProfileChange};
use crate::services::profile_record_service::{
    self, EducationRecordInput, WorkExperienceRecordInput,
};
use crate::services::rbac_admin_service;
use crate::services::separation_service;
use rust_decimal::Decimal;

use crate::entities::d0008_document_system::{document_type, employee_document};
use crate::entities::d0017_onboarding_offboarding::onboarding_checklist;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

async fn enrich_employee_document_dto(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    m: employee_document::Model,
) -> Result<EmployeeDocumentDto> {
    let dto = EmployeeDocumentDto::from(m.clone());
    let (ofn, ub) = if let Some(fid) = m.file_storage_id {
        let map = document_service::map_file_storage_rows(db, tenant_id, &[fid])
            .await
            .map_err(KabiPayError::into_graphql)?;
        let fs = map.get(&fid);
        (
            fs.and_then(|f| f.original_filename.clone()),
            fs.and_then(|f| f.uploaded_by),
        )
    } else {
        (None, None)
    };
    let dt_map = document_service::map_document_type_rows(db, tenant_id, &[m.document_type_id])
        .await
        .map_err(KabiPayError::into_graphql)?;
    let dt = dt_map.get(&m.document_type_id);
    Ok(dto.with_file_and_type(
        ofn,
        ub,
        dt.map(|d| d.name.clone()),
        dt.and_then(|d| d.category.clone()),
    ))
}

async fn education_dto(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    row: crate::entities::d0050_employee_self_service::employee_education::Model,
) -> Result<EmployeeEducationDto> {
    let evidence_ids = profile_record_service::education_evidence_ids(db, tenant_id, row.id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    Ok(EmployeeEducationDto::from_model(row, evidence_ids))
}

async fn work_experience_dto(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    row: crate::entities::d0050_employee_self_service::employee_work_experience::Model,
) -> Result<EmployeeWorkExperienceDto> {
    let evidence_ids = profile_record_service::work_evidence_ids(db, tenant_id, row.id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    Ok(EmployeeWorkExperienceDto::from_model(row, evidence_ids))
}

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn opt_uuid(id: &Option<ID>, field: &'static str) -> Result<Option<Uuid>> {
    match id {
        None => Ok(None),
        Some(i) => Ok(Some(parse_uuid(i, field)?)),
    }
}

const MIN_PASSWORD_LEN: usize = 8;

fn validate_admin_password(raw: String, field: &'static str) -> Result<String> {
    let trimmed = raw.trim().to_string();
    if trimmed.len() < MIN_PASSWORD_LEN {
        return Err(KabiPayError::Validation(format!(
            "{field} must be at least {MIN_PASSWORD_LEN} characters"
        ))
        .into_graphql());
    }
    Ok(trimmed)
}

fn parse_role_ids(role_ids: Option<Vec<ID>>) -> Result<Vec<Uuid>> {
    let mut parsed = Vec::new();
    for id in role_ids.unwrap_or_default() {
        parsed.push(parse_uuid(&id, "roleId")?);
    }
    Ok(parsed)
}

async fn hash_password_async(plaintext: String) -> Result<String> {
    tokio::task::spawn_blocking(move || password::hash(&plaintext))
        .await
        .map_err(|error| {
            KabiPayError::Internal(format!("password hashing task failed: {error}")).into_graphql()
        })?
        .map_err(KabiPayError::into_graphql)
}

/// Enforce RBAC for directory-changing employee writes.
///
/// - Valid **client JWT** must include `employee:write` or `employee:manage`, **or** role
///   `HR_ADMIN` / `TENANT_ADMIN` / `ORG_ADMIN` (from loaded `user_role` at login).
/// - **Dev only:** set `KABIPAY_EMPLOYEE_MUTATION_HEADER_OK=1` to allow unauthenticated
///   `x-tenant-id` (no claims) for local automation — never in production.
fn require_employee_mutation_rbac(ctx: &Context<'_>) -> Result<()> {
    if ctx.data_opt::<ClientClaims>().is_none() {
        if std::env::var("KABIPAY_EMPLOYEE_MUTATION_HEADER_OK").as_deref() == Ok("1") {
            return Ok(());
        }
        return Err(KabiPayError::Unauthorised.into_graphql());
    }
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_employee_directory() {
        return Err(KabiPayError::Forbidden(
            "employee:write, employee:manage, or HR_ADMIN / TENANT_ADMIN role required".into(),
        )
        .into_graphql());
    }
    Ok(())
}

/// Offboarding HR mutations: directory admins **or** `onboarding:manage`.
fn require_offboarding_hr_mutation(ctx: &Context<'_>) -> Result<()> {
    if ctx.data_opt::<ClientClaims>().is_none() {
        if std::env::var("KABIPAY_EMPLOYEE_MUTATION_HEADER_OK").as_deref() == Ok("1") {
            return Ok(());
        }
        return Err(KabiPayError::Unauthorised.into_graphql());
    }
    let claims = require_client_claims(ctx)?;
    if claims.can_manage_employee_directory() || claims.can_manage_onboarding_tenant() {
        return Ok(());
    }
    Err(
        KabiPayError::Forbidden("employee:write or onboarding:manage required".into())
            .into_graphql(),
    )
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn create_employee(
        &self,
        ctx: &Context<'_>,
        input: CreateEmployeeInput,
    ) -> Result<EmployeeDto> {
        require_employee_mutation_rbac(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;

        let login_account = input.login_account;
        if login_account.is_some() {
            require_tenant_rbac_admin(ctx)?;
        }

        let data = NewEmployee {
            employee_code: input.employee_code,
            first_name: input.first_name,
            last_name: input.last_name,
            date_of_joining: input.date_of_joining,
            department_id: opt_uuid(&input.department_id, "departmentId")?,
            designation_id: opt_uuid(&input.designation_id, "designationId")?,
            reporting_manager_id: opt_uuid(&input.reporting_manager_id, "reportingManagerId")?,
            employment_type: input.employment_type,
            status: input
                .status
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "ACTIVE".into()),
            user_id: opt_uuid(&input.user_id, "userId")?,
        };

        let m = if let Some(login) = login_account {
            let initial_password =
                validate_admin_password(login.initial_password, "initialPassword")?;
            let password_hash = hash_password_async(initial_password).await?;
            let role_ids = parse_role_ids(login.role_ids)?;
            employee_service::create_with_login(
                &db,
                tenant_id,
                data,
                employee_service::NewLoginAccount {
                    username: login.username,
                    email: login.email,
                    password_hash,
                    role_ids,
                },
            )
            .await
            .map_err(KabiPayError::into_graphql)?
        } else {
            employee_service::create(&db, tenant_id, data)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        Ok(EmployeeDto::from(m))
    }

    async fn provision_employee_login(
        &self,
        ctx: &Context<'_>,
        input: ProvisionEmployeeLoginInput,
    ) -> Result<EmployeeDto> {
        require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let initial_password = validate_admin_password(input.initial_password, "initialPassword")?;
        let password_hash = hash_password_async(initial_password).await?;
        let role_ids = parse_role_ids(input.role_ids)?;
        let m = employee_service::provision_login(
            &db,
            tenant_id,
            employee_id,
            employee_service::NewLoginAccount {
                username: input.username,
                email: input.email,
                password_hash,
                role_ids,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeDto::from(m))
    }

    async fn reset_employee_password(
        &self,
        ctx: &Context<'_>,
        input: ResetEmployeePasswordInput,
    ) -> Result<bool> {
        require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let new_password = validate_admin_password(input.new_password, "newPassword")?;
        let password_hash = hash_password_async(new_password).await?;
        employee_service::reset_linked_user_password(&db, tenant_id, employee_id, password_hash)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn update_employee(
        &self,
        ctx: &Context<'_>,
        input: UpdateEmployeeInput,
    ) -> Result<EmployeeDto> {
        require_employee_mutation_rbac(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.id, "id")?;
        let reporting_manager_id = match input.reporting_manager_id {
            None => None,
            Some(None) => Some(None),
            Some(Some(ref id)) => Some(Some(parse_uuid(id, "reportingManagerId")?)),
        };
        let patch = EmployeePatch {
            first_name: input.first_name,
            last_name: input.last_name,
            department_id: opt_uuid(&input.department_id, "departmentId")?,
            designation_id: opt_uuid(&input.designation_id, "designationId")?,
            reporting_manager_id,
            employment_type: input.employment_type,
            status: input.status,
            user_id: opt_uuid(&input.user_id, "userId")?,
        };
        let m = employee_service::update(&db, tenant_id, eid, patch)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeDto::from(m))
    }

    /// HR: set or update monthly salary for an employee (`employment_history`), effective from a date.
    async fn set_employee_compensation(
        &self,
        ctx: &Context<'_>,
        input: SetEmployeeCompensationInput,
    ) -> Result<EmploymentHistoryRecordDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let monthly = Decimal::from_str_exact(input.monthly_salary.trim()).map_err(|e| {
            KabiPayError::Validation(format!("monthlySalary: invalid decimal ({e})")).into_graphql()
        })?;
        let m = employment_history_service::set_monthly_salary(
            &db,
            tenant_id,
            claims.sub,
            eid,
            monthly,
            input.effective_from,
            input.change_reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmploymentHistoryRecordDto::from(m))
    }

    /// Upload a file to local `KABIPAY_LOCAL_FILE_ROOT` or object storage and attach `employee_document`.
    /// **Directory/HR** uploads are **`APPROVED`** immediately; others start **`PENDING`** for HR review.
    async fn upload_employee_document(
        &self,
        ctx: &Context<'_>,
        input: UploadEmployeeDocumentInput,
    ) -> Result<EmployeeDocumentDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        let dtid = parse_uuid(&input.document_type_id, "documentTypeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;

        if document_type::Entity::find_by_id(dtid)
            .filter(document_type::Column::TenantId.eq(tenant_id))
            .filter(document_type::Column::IsDeleted.eq(false))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .is_none()
        {
            return Err(KabiPayError::NotFound {
                entity: "documentType",
                id: dtid.to_string(),
            }
            .into_graphql());
        }

        let uploader = Some(require_client_claims(ctx)?.sub);

        let hr_auto = ctx
            .data_opt::<ClientClaims>()
            .map(|c| c.can_manage_employee_directory())
            .unwrap_or(false);

        let bytes = STANDARD
            .decode(input.content_base64.as_bytes())
            .map_err(|e| KabiPayError::Validation(format!("contentBase64: {e}")).into_graphql())?;

        let m = document_file_service::upload_employee_document(
            &db,
            tenant_id,
            eid,
            dtid,
            uploader,
            input.file_name,
            input.mime_type,
            bytes,
            hr_auto,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        enrich_employee_document_dto(&db, tenant_id, m).await
    }

    /// Upload a tenant-scoped file and return its reusable `file_storage.id`.
    async fn upload_tenant_file(
        &self,
        ctx: &Context<'_>,
        input: UploadTenantFileInput,
    ) -> Result<UploadedTenantFileDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uploader = Some(require_client_claims(ctx)?.sub);
        let bytes = STANDARD
            .decode(input.content_base64.as_bytes())
            .map_err(|e| KabiPayError::Validation(format!("contentBase64: {e}")).into_graphql())?;
        let m = document_file_service::upload_tenant_file(
            &db,
            tenant_id,
            uploader,
            input.file_name,
            input.mime_type,
            bytes,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(UploadedTenantFileDto::from(m))
    }

    /// Demographics + emergency contact. Employee may edit **self**; HR may edit anyone in scope.
    async fn update_employee_personal_profile(
        &self,
        ctx: &Context<'_>,
        input: UpdateEmployeePersonalProfileInput,
    ) -> Result<EmployeeDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let viewer = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer != Some(eid) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write to update another profile".into(),
            )
            .into_graphql());
        }
        if viewer == Some(eid)
            && !claims.can_manage_employee_directory()
            && (input.first_name.is_some()
                || input.last_name.is_some()
                || input.date_of_birth.is_some())
        {
            return Err(KabiPayError::Forbidden(
                "legal name and date of birth changes require an HR-reviewed profile change request"
                    .into(),
            )
            .into_graphql());
        }
        let patch = PersonalProfilePatch {
            first_name: input.first_name,
            last_name: input.last_name,
            date_of_birth: input.date_of_birth,
            gender: input.gender,
            nationality: input.nationality,
            blood_group: input.blood_group,
            emergency_contact_name: input.emergency_contact_name,
            emergency_contact_phone: input.emergency_contact_phone,
            emergency_contact_relation: input.emergency_contact_relation,
        };
        let m = employee_service::update_personal_profile(&db, tenant_id, eid, patch)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let dto = EmployeeDto::from(m);
        let mut enriched = enrich_employee_dtos(&db, tenant_id, vec![dto]).await?;
        enriched.pop().ok_or_else(|| {
            KabiPayError::Internal("failed to enrich employee dto".into()).into_graphql()
        })
    }

    /// Direct self-service fields that do not change legal identity or organization assignment.
    async fn update_employee_self_service_profile(
        &self,
        ctx: &Context<'_>,
        input: UpdateEmployeeSelfServiceProfileInput,
    ) -> Result<EmployeeDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write".into(),
            )
            .into_graphql());
        }
        let updated = employee_service::update_self_service_profile(
            &db,
            tenant_id,
            employee_id,
            SelfServiceProfilePatch {
                personal_phone: input.personal_phone,
                current_address: input.current_address,
                permanent_address: input.permanent_address,
                gender: input.gender,
                nationality: input.nationality,
                blood_group: input.blood_group,
                emergency_contact_name: input.emergency_contact_name,
                emergency_contact_phone: input.emergency_contact_phone,
                emergency_contact_relation: input.emergency_contact_relation,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let mut enriched = enrich_employee_dtos(&db, tenant_id, vec![EmployeeDto::from(updated)]).await?;
        enriched.pop().ok_or_else(|| {
            KabiPayError::Internal("updated employee profile missing".into()).into_graphql()
        })
    }

    async fn submit_employee_profile_change(
        &self,
        ctx: &Context<'_>,
        input: SubmitEmployeeProfileChangeInput,
    ) -> Result<EmployeeProfileChangeRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write".into(),
            )
            .into_graphql());
        }
        let supporting_document_id = input
            .supporting_document_id
            .as_ref()
            .map(|id| parse_uuid(id, "supportingDocumentId"))
            .transpose()?;
        let row = profile_change_service::submit_request(
            &db,
            tenant_id,
            employee_id,
            claims.sub,
            NewProfileChange {
                request_type: input.request_type,
                first_name: input.first_name,
                last_name: input.last_name,
                date_of_birth: input.date_of_birth,
                pan_number: input.pan_number,
                aadhaar_number: input.aadhaar_number,
                bank_name: input.bank_name,
                account_number: input.account_number,
                ifsc_code: input.ifsc_code,
                account_type: input.account_type,
                supporting_document_id,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeProfileChangeRequestDto::from(row))
    }

    async fn cancel_employee_profile_change(
        &self,
        ctx: &Context<'_>,
        request_id: ID,
    ) -> Result<EmployeeProfileChangeRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let request_id = parse_uuid(&request_id, "requestId")?;
        let existing = profile_change_service::find_request(&db, tenant_id, request_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "employeeProfileChangeRequest",
                id: request_id.to_string(),
            }
            .into_graphql())?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, existing.employee_id).await?;
        let row = profile_change_service::cancel_request(
            &db,
            tenant_id,
            request_id,
            claims.sub,
            claims.can_manage_employee_directory(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeProfileChangeRequestDto::from(row))
    }

    async fn resolve_employee_profile_change(
        &self,
        ctx: &Context<'_>,
        request_id: ID,
        approved: bool,
        rejection_reason: Option<String>,
    ) -> Result<EmployeeProfileChangeRequestDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let request_id = parse_uuid(&request_id, "requestId")?;
        let existing = profile_change_service::find_request(&db, tenant_id, request_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "employeeProfileChangeRequest",
                id: request_id.to_string(),
            }
            .into_graphql())?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, existing.employee_id).await?;
        let row = profile_change_service::resolve_request(
            &db,
            tenant_id,
            request_id,
            claims.sub,
            approved,
            rejection_reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeProfileChangeRequestDto::from(row))
    }

    async fn upsert_employee_education(
        &self,
        ctx: &Context<'_>,
        input: UpsertEmployeeEducationInput,
    ) -> Result<EmployeeEducationDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "education records may only be changed by the employee or HR".into(),
            )
            .into_graphql());
        }
        let record_id = input
            .id
            .as_ref()
            .map(|id| parse_uuid(id, "educationId"))
            .transpose()?;
        let row = profile_record_service::save_education(
            &db,
            tenant_id,
            employee_id,
            record_id,
            EducationRecordInput {
                education_level: input.education_level,
                qualification: input.qualification,
                field_of_study: input.field_of_study,
                institution: input.institution,
                board_university: input.board_university,
                start_date: input.start_date,
                completion_year: input.completion_year,
                grade_score: input.grade_score,
                description: input.description,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        education_dto(&db, tenant_id, row).await
    }

    async fn delete_employee_education(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        education_id: ID,
    ) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "education records may only be deleted by the employee or HR".into(),
            )
            .into_graphql());
        }
        profile_record_service::delete_education(
            &db,
            tenant_id,
            employee_id,
            parse_uuid(&education_id, "educationId")?,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn link_employee_education_evidence(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        education_id: ID,
        employee_document_id: ID,
    ) -> Result<EmployeeEducationDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "education evidence may only be linked by the employee or HR".into(),
            )
            .into_graphql());
        }
        let row = profile_record_service::link_education_evidence(
            &db,
            tenant_id,
            employee_id,
            parse_uuid(&education_id, "educationId")?,
            parse_uuid(&employee_document_id, "employeeDocumentId")?,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        education_dto(&db, tenant_id, row).await
    }

    async fn resolve_employee_education(
        &self,
        ctx: &Context<'_>,
        education_id: ID,
        approved: bool,
        rejection_reason: Option<String>,
    ) -> Result<EmployeeEducationDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let education_id = parse_uuid(&education_id, "educationId")?;
        let existing = profile_record_service::find_education(&db, tenant_id, education_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "employeeEducation",
                id: education_id.to_string(),
            }
            .into_graphql())?;
        if resolve_client_employee_id(ctx, &db, tenant_id).await.ok() == Some(existing.employee_id) {
            return Err(KabiPayError::Forbidden(
                "employees cannot verify their own education evidence".into(),
            )
            .into_graphql());
        }
        assert_employee_in_data_scope(ctx, &db, tenant_id, existing.employee_id).await?;
        let row = profile_record_service::review_education(
            &db,
            tenant_id,
            education_id,
            claims.sub,
            approved,
            rejection_reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        education_dto(&db, tenant_id, row).await
    }

    async fn upsert_employee_work_experience(
        &self,
        ctx: &Context<'_>,
        input: UpsertEmployeeWorkExperienceInput,
    ) -> Result<EmployeeWorkExperienceDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "work experience may only be changed by the employee or HR".into(),
            )
            .into_graphql());
        }
        let record_id = input
            .id
            .as_ref()
            .map(|id| parse_uuid(id, "workExperienceId"))
            .transpose()?;
        let row = profile_record_service::save_work_experience(
            &db,
            tenant_id,
            employee_id,
            record_id,
            WorkExperienceRecordInput {
                company: input.company,
                role_title: input.role_title,
                employment_type: input.employment_type,
                location: input.location,
                start_date: input.start_date,
                end_date: input.end_date,
                is_current: input.is_current,
                description: input.description,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        work_experience_dto(&db, tenant_id, row).await
    }

    async fn delete_employee_work_experience(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        work_experience_id: ID,
    ) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "work experience may only be deleted by the employee or HR".into(),
            )
            .into_graphql());
        }
        profile_record_service::delete_work_experience(
            &db,
            tenant_id,
            employee_id,
            parse_uuid(&work_experience_id, "workExperienceId")?,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn link_employee_work_experience_evidence(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        work_experience_id: ID,
        employee_document_id: ID,
    ) -> Result<EmployeeWorkExperienceDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, employee_id).await?;
        let viewer_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_id != Some(employee_id) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "work evidence may only be linked by the employee or HR".into(),
            )
            .into_graphql());
        }
        let row = profile_record_service::link_work_evidence(
            &db,
            tenant_id,
            employee_id,
            parse_uuid(&work_experience_id, "workExperienceId")?,
            parse_uuid(&employee_document_id, "employeeDocumentId")?,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        work_experience_dto(&db, tenant_id, row).await
    }

    async fn resolve_employee_work_experience(
        &self,
        ctx: &Context<'_>,
        work_experience_id: ID,
        approved: bool,
        rejection_reason: Option<String>,
    ) -> Result<EmployeeWorkExperienceDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let work_experience_id = parse_uuid(&work_experience_id, "workExperienceId")?;
        let existing = profile_record_service::find_work_experience(
            &db,
            tenant_id,
            work_experience_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeWorkExperience",
            id: work_experience_id.to_string(),
        }
        .into_graphql())?;
        if resolve_client_employee_id(ctx, &db, tenant_id).await.ok() == Some(existing.employee_id) {
            return Err(KabiPayError::Forbidden(
                "employees cannot verify their own work experience evidence".into(),
            )
            .into_graphql());
        }
        assert_employee_in_data_scope(ctx, &db, tenant_id, existing.employee_id).await?;
        let row = profile_record_service::review_work_experience(
            &db,
            tenant_id,
            work_experience_id,
            claims.sub,
            approved,
            rejection_reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        work_experience_dto(&db, tenant_id, row).await
    }

    /// Upsert the primary bank row (self or **`employee:write`**).
    async fn upsert_employee_primary_bank(
        &self,
        ctx: &Context<'_>,
        input: UpsertEmployeePrimaryBankInput,
    ) -> Result<EmployeeBankAccountDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let viewer = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer != Some(eid) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write".into(),
            )
            .into_graphql());
        }
        let m = profile_extras_service::upsert_primary_bank(
            &db,
            tenant_id,
            eid,
            input.bank_name,
            input.account_number,
            input.ifsc_code,
            input.account_type,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeBankAccountDto::from_model(&m))
    }

    /// Upsert primary PAN (self or **`employee:write`**). Clears verification until HR re-verifies.
    async fn upsert_employee_primary_pan(
        &self,
        ctx: &Context<'_>,
        input: UpsertEmployeePrimaryPanInput,
    ) -> Result<EmployeePanRecordDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let viewer = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer != Some(eid) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write".into(),
            )
            .into_graphql());
        }
        let m = profile_extras_service::upsert_primary_pan(&db, tenant_id, eid, input.pan_number)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeePanRecordDto::from_model(&m))
    }

    /// Upsert primary Aadhaar last‑4 (self or **`employee:write`**). Clears verification.
    async fn upsert_employee_primary_aadhaar(
        &self,
        ctx: &Context<'_>,
        input: UpsertEmployeePrimaryAadhaarInput,
    ) -> Result<EmployeeAadhaarRecordDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        assert_employee_in_data_scope(ctx, &db, tenant_id, eid).await?;
        let viewer = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer != Some(eid) && !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "use your own employee id or employee:write".into(),
            )
            .into_graphql());
        }
        let m = profile_extras_service::upsert_primary_aadhaar(
            &db,
            tenant_id,
            eid,
            input.aadhaar_number,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeAadhaarRecordDto::from_model(&m))
    }

    /// HR: approve or reject a **`PENDING`** employee document.
    async fn resolve_employee_document(
        &self,
        ctx: &Context<'_>,
        employee_document_id: ID,
        approved: bool,
    ) -> Result<EmployeeDocumentDto> {
        require_employee_mutation_rbac(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let doc_id = parse_uuid(&employee_document_id, "employeeDocumentId")?;
        let existing = employee_document::Entity::find_by_id(doc_id)
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
        assert_employee_in_data_scope(ctx, &db, tenant_id, existing.employee_id).await?;
        let m = document_service::resolve_employee_document_status(
            &db, tenant_id, doc_id, approved, claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        enrich_employee_document_dto(&db, tenant_id, m).await
    }

    /// Mark an onboarding checklist row complete or incomplete. Employees may update **their own**
    /// tasks; HR / directory roles may update tasks for employees in their data scope.
    async fn set_onboarding_checklist_item_completed(
        &self,
        ctx: &Context<'_>,
        checklist_item_id: ID,
        is_completed: bool,
    ) -> Result<OnboardingChecklistItemDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let task_id = parse_uuid(&checklist_item_id, "checklistItemId")?;
        let row = onboarding_checklist::Entity::find_by_id(task_id)
            .filter(onboarding_checklist::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "onboardingChecklistItem",
                    id: task_id.to_string(),
                }
                .into_graphql()
            })?;
        let viewer = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let hr_or_onboarding =
            claims.can_manage_employee_directory() || claims.can_manage_onboarding_tenant();
        if hr_or_onboarding {
            assert_employee_in_data_scope(ctx, &db, tenant_id, row.employee_id).await?;
        } else if claims.can_use_onboarding_self_service() {
            if row.employee_id != viewer {
                return Err(KabiPayError::Forbidden(
                    "you can only update your own onboarding tasks".into(),
                )
                .into_graphql());
            }
        } else {
            return Err(
                KabiPayError::Forbidden("onboarding permission required".into()).into_graphql(),
            );
        }
        let m = onboarding_service::set_task_completed(&db, tenant_id, task_id, is_completed)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(OnboardingChecklistItemDto::from(m))
    }

    /// File a separation / exit request (self-service, or HR on behalf of another employee).
    async fn submit_separation(
        &self,
        ctx: &Context<'_>,
        input: SubmitSeparationInput,
    ) -> Result<SeparationDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let target_emp = if let Some(ref eid) = input.employee_id {
            let e = parse_uuid(eid, "employeeId")?;
            if claims.can_manage_employee_directory() || claims.can_manage_onboarding_tenant() {
                e
            } else {
                return Err(KabiPayError::Forbidden(
                    "only HR can file separation for another employee".into(),
                )
                .into_graphql());
            }
        } else {
            if !claims.can_use_onboarding_self_service() {
                return Err(
                    KabiPayError::Forbidden("onboarding:self permission required".into())
                        .into_graphql(),
                );
            }
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        employee_service::find_by_id(&db, tenant_id, target_emp)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "employee",
                    id: target_emp.to_string(),
                }
                .into_graphql()
            })?;
        let m = separation_service::insert_separation(
            &db,
            tenant_id,
            target_emp,
            input.separation_type,
            input.resignation_date,
            input.last_working_date,
            input.reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(SeparationDto::from(m))
    }

    /// Approve a pending separation (HR / directory roles — same gate as `createEmployee`).
    async fn approve_separation(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<SeparationDto> {
        require_offboarding_hr_mutation(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&separation_id, "separationId")?;
        let m = separation_service::resolve_separation(&db, tenant_id, sid, true, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(SeparationDto::from(m))
    }

    /// Reject a pending separation (HR / directory roles).
    async fn reject_separation(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<SeparationDto> {
        require_offboarding_hr_mutation(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&separation_id, "separationId")?;
        let m = separation_service::resolve_separation(&db, tenant_id, sid, false, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(SeparationDto::from(m))
    }

    /// HR: fill FNF component amounts (while status is DRAFT). Net payable is recalculated.
    async fn upsert_fnf_settlement(
        &self,
        ctx: &Context<'_>,
        input: UpsertFnfSettlementInput,
    ) -> Result<FnfSettlementDto> {
        require_offboarding_hr_mutation(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&input.separation_id, "separationId")?;
        let m = offboarding_fnf_service::upsert_fnf_settlement(
            &db,
            tenant_id,
            sid,
            &input.leave_encashment,
            &input.gratuity_amount,
            &input.bonus_payable,
            &input.recovery_amount,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(FnfSettlementDto::from(m))
    }

    /// HR: mark FNF as PROCESSED (no further amount edits).
    async fn finalize_fnf_settlement(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<FnfSettlementDto> {
        require_offboarding_hr_mutation(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&separation_id, "separationId")?;
        let m = offboarding_fnf_service::finalize_fnf_settlement(&db, tenant_id, sid, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(FnfSettlementDto::from(m))
    }

    /// HR: create DRAFT FNF + default clearance for an `APPROVED` separation (e.g. legacy row before auto-seed).
    async fn ensure_separation_offboarding_artifacts(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<bool> {
        require_offboarding_hr_mutation(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&separation_id, "separationId")?;
        offboarding_fnf_service::backfill_approved_artifacts(&db, tenant_id, sid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// HR: toggle a department clearance line.
    async fn set_clearance_item_cleared(
        &self,
        ctx: &Context<'_>,
        clearance_id: ID,
        is_cleared: bool,
    ) -> Result<ClearanceChecklistItemDto> {
        require_offboarding_hr_mutation(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&clearance_id, "clearanceId")?;
        let m = offboarding_fnf_service::set_clearance_cleared(
            &db, tenant_id, cid, is_cleared, claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ClearanceChecklistItemDto::from(m))
    }

    /// Replace `role_permission` rows for a role (full matrix row).
    async fn set_role_permissions(
        &self,
        ctx: &Context<'_>,
        role_id: ID,
        permission_ids: Vec<ID>,
    ) -> Result<bool> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rid = parse_uuid(&role_id, "roleId")?;
        let mut pids = Vec::with_capacity(permission_ids.len());
        for id in permission_ids {
            pids.push(parse_uuid(&id, "permissionId")?);
        }
        rbac_admin_service::set_role_permissions(&db, tenant_id, rid, pids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Replace `user_role` rows for a user. Caller must re-login to refresh JWT claims.
    async fn set_user_roles(
        &self,
        ctx: &Context<'_>,
        user_id: ID,
        role_ids: Vec<ID>,
    ) -> Result<bool> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uid = parse_uuid(&user_id, "userId")?;
        let mut rids = Vec::with_capacity(role_ids.len());
        for id in role_ids {
            rids.push(parse_uuid(&id, "roleId")?);
        }
        rbac_admin_service::set_user_roles(&db, tenant_id, uid, rids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Replace `permission_scope` rows for a role (list-filter scopes).
    async fn set_role_permission_scopes(
        &self,
        ctx: &Context<'_>,
        role_id: ID,
        scopes: Vec<PermissionScopeAssignmentInput>,
    ) -> Result<bool> {
        let _ = require_tenant_rbac_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rid = parse_uuid(&role_id, "roleId")?;
        let tuples: Vec<(String, String, String)> = scopes
            .into_iter()
            .map(|s| (s.resource, s.action, s.scope_type))
            .collect();
        rbac_admin_service::set_role_permission_scopes(&db, tenant_id, rid, tuples)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }
}
