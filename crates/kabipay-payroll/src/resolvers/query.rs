//! Root query resolvers for kabipay-payroll.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_context, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::PERM_EMPLOYEE_READ,
    file_download_token::{file_download_claims, public_employee_file_download_url},
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::resolvers::types::{
    PayrollArrearDto, PayrollComplianceSettingDto, PayrollCycleDto, PayslipDetailDto,
    SalaryBreakupLineDto, SalaryBreakupPreviewDto, SalaryComponentDto, SalaryStructureComponentDto,
    SalaryStructureDto,
};
use crate::services::arrear_service;
use crate::services::payroll_service;

pub(crate) fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn payroll_health(&self) -> &'static str {
        "ok"
    }

    /// List salary components (earnings/deductions) for the caller's tenant.
    async fn salary_components(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<SalaryComponentDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = payroll_service::list_components(&db, tenant_id, active_only, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(SalaryComponentDto::from).collect())
    }

    async fn salary_structures(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<SalaryStructureDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() && !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden(
                    "salary structures require payroll:statutory_export or compensation:manage".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = payroll_service::list_salary_structures(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|(structure, components)| {
                let component_dtos = components
                    .into_iter()
                    .map(|(line, component)| SalaryStructureComponentDto::from_parts(line, component))
                    .collect();
                SalaryStructureDto::from_head(structure, component_dtos)
            })
            .collect())
    }

    async fn employee_salary_breakup_preview(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        as_of: Option<chrono::NaiveDate>,
    ) -> Result<Option<SalaryBreakupPreviewDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&employee_id, "employeeId")?;
        let viewer_eid = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        if viewer_eid != Some(eid)
            && !claims.can_export_payroll_statutory()
            && !claims.can_manage_compensation_admin()
        {
            return Err(
                KabiPayError::Forbidden(
                    "salary breakup is private to the employee and payroll/HR".into(),
                )
                .into_graphql(),
            );
        }
        let as_of = as_of.unwrap_or_else(|| chrono::Utc::now().date_naive());
        let Some(preview) = payroll_service::preview_employee_salary_breakup(&db, tenant_id, eid, as_of)
            .await
            .map_err(KabiPayError::into_graphql)?
        else {
            return Ok(None);
        };
        Ok(Some(SalaryBreakupPreviewDto {
            employee_id: ID(preview.employee_id.to_string()),
            employee_salary_structure_id: preview
                .employee_salary_structure_id
                .map(|id| ID(id.to_string())),
            annual_ctc: preview.annual_ctc.to_string(),
            monthly_gross: preview.monthly_gross.to_string(),
            monthly_deductions: preview.monthly_deductions.to_string(),
            monthly_net_before_statutory: preview.monthly_net_before_statutory.to_string(),
            lines: preview
                .lines
                .into_iter()
                .map(|line| SalaryBreakupLineDto {
                    salary_component_id: ID(line.salary_component_id.to_string()),
                    component_name: line.component_name,
                    component_code: line.component_code,
                    component_type: line.component_type,
                    calculation_basis: line.calculation_basis,
                    calculation_value: line.calculation_value.to_string(),
                    annual_amount: line.annual_amount.to_string(),
                    monthly_amount: line.monthly_amount.to_string(),
                    is_override: line.is_override,
                })
                .collect(),
        }))
    }

    /// List payroll cycles for the caller's tenant, most recent first.
    async fn payroll_cycles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 24)] limit: u64,
    ) -> Result<Vec<PayrollCycleDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = payroll_service::list_cycles(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(PayrollCycleDto::from).collect())
    }

    /// Employer TAN, payslip branding, component codes (optional row per tenant).
    /// Readable by any authenticated client so employees can render branded payslips; **`upsertPayrollComplianceSetting`**
    /// remains restricted to statutory-export / HR roles.
    async fn payroll_compliance_setting(&self, ctx: &Context<'_>) -> Result<Option<PayrollComplianceSettingDto>> {
        require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let row = payroll_service::find_payroll_compliance_setting(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(row.map(PayrollComplianceSettingDto::from))
    }

    /// HMAC URL for **`GET /files/employee-document?token=…`** on **kabipay-employee** (same as document downloads).
    /// Only issued when **`fileStorageId`** equals **`payroll_compliance_setting.payslip_logo_file_storage_id`**.
    async fn payslip_logo_signed_read_url(
        &self,
        ctx: &Context<'_>,
        file_storage_id: ID,
        #[graphql(default = 600)] ttl_seconds: i32,
    ) -> Result<String> {
        require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let wanted = parse_uuid(&file_storage_id, "fileStorageId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let compliance = payroll_service::find_payroll_compliance_setting(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some(row) = compliance else {
            return Err(
                KabiPayError::Validation("payroll compliance setting is not configured for this tenant".into())
                    .into_graphql(),
            );
        };
        let Some(logo_id) = row.payslip_logo_file_storage_id else {
            return Err(
                KabiPayError::Validation("tenant has no payslip logo configured".into()).into_graphql(),
            );
        };
        if logo_id != wanted {
            return Err(
                KabiPayError::Forbidden("file id does not match the tenant payslip logo".into()).into_graphql(),
            );
        }
        let fs_row = file_storage::Entity::find_by_id(logo_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "fileStorage",
                    id: logo_id.to_string(),
                }
                .into_graphql()
            })?;
        let ttl = ttl_seconds.clamp(60, 86_400) as i64;
        let claims = file_download_claims(tenant_id, logo_id, fs_row.mime_type.clone(), ttl);
        public_employee_file_download_url(&claims).map_err(KabiPayError::into_graphql)
    }

    /// One payslip with `lines` = `payslip_component` rows.
    async fn payslip(&self, ctx: &Context<'_>, id: ID) -> Result<Option<PayslipDetailDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = parse_uuid(&id, "id")?;
        let row = payroll_service::find_payslip_detail(&db, tenant_id, sid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some((p, c)) = row else {
            return Ok(None);
        };
        let scope = data_scope_from_context(ctx, PERM_EMPLOYEE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if !filt.allows_employee(p.employee_id) {
            return Ok(None);
        }
        Ok(Some(PayslipDetailDto::from_head(p, c)))
    }

    /// When `employeeId` is omitted, uses the signed-in user’s employee id from the JWT
    /// (or `user` → `employee` link). Pass `employeeId` to view a specific person (e.g. HR).
    async fn payslips(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 24)] limit: u64,
    ) -> Result<Vec<PayslipDetailDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let empl = if let Some(id) = &employee_id {
            parse_uuid(id, "employeeId")?
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        let scope = data_scope_from_context(ctx, PERM_EMPLOYEE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if !filt.allows_employee(empl) {
            return Ok(vec![]);
        }
        let list = payroll_service::list_payslips(&db, tenant_id, Some(empl), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let ids: Vec<Uuid> = list.iter().map(|p| p.id).collect();
        let lines = payroll_service::payslip_lines_by_payslip_ids(&db, tenant_id, &ids)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let out: Vec<PayslipDetailDto> = list
            .into_iter()
            .map(|p| {
                let c = lines.get(&p.id).cloned().unwrap_or_default();
                PayslipDetailDto::from_head(p, c)
            })
            .collect();
        Ok(out)
    }

    /// **India — monthly TDS summary (CSV).** All payslips for the payroll cycle matching
    /// `month` + `calendar year`. Requires `payroll:statutory_export`.
    /// Stub for statutory filing prep — not a filed Form 24Q; values come from `payslip.tds_amount`.
    async fn india_tds_monthly_summary_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll statutory export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_tds_monthly_summary_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India — monthly PF / ESI summary (CSV).** Payslip statutory columns (`pfEmployee`, `esiEmployee`, UAN, ESIC, …)
    /// for every payslip in the payroll cycle matching `month` + `year`. Same RBAC as TDS export; not ECR / challan output.
    async fn india_pf_esi_monthly_summary_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll statutory export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_pf_esi_monthly_summary_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **Payroll — bank transfer list (CSV).** Net pay and primary `employee_bank` for each payslip
    /// in the cycle for `month` + `year`. Same RBAC as India statutory exports; not a specific bank’s
    /// upload file format.
    async fn payroll_bank_transfer_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll bank transfer export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::payroll_bank_transfer_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India — NEFT / bulk salary credit prep (CSV).** Multi-beneficiary style columns (IFSC, account,
    /// narration, optional value date from cycle). Same RBAC as other payroll bank/statutory exports.
    async fn payroll_india_bulk_neft_credit_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll bulk credit export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::payroll_india_bulk_neft_credit_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India FY — per-employee aggregated payslip totals (CSV).** Rolls up all payslips in cycles whose
    /// India FY matches `fyStartYear`. Stub for annual compliance prep (e.g. Form 16). Same RBAC as TDS export.
    async fn india_fy_payroll_employee_totals_csv(
        &self,
        ctx: &Context<'_>,
        fy_start_year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll FY totals export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_fy_payroll_employee_totals_csv(&db, tenant_id, fy_start_year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India FY quarter — employee totals (CSV).** Same measures as **`indiaFyPayrollEmployeeTotalsCsv`**, scoped to FY **Q1**–**Q4** months only — quarterly reconciliation prep (e.g. 24Q), not filed layout.
    async fn india_fy_quarter_payroll_employee_totals_csv(
        &self,
        ctx: &Context<'_>,
        fy_start_year: i32,
        quarter: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll FY quarter totals export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_fy_quarter_payroll_employee_totals_csv(
            &db,
            tenant_id,
            fy_start_year,
            quarter,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    /// **India FY — Form 16 Part B prep (stub CSV).** Aggregates with Part B–oriented headers; blank employer TAN/name placeholders.
    async fn india_form16_part_b_fy_prep_stub_csv(
        &self,
        ctx: &Context<'_>,
        fy_start_year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll Form 16 Part B prep export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_form16_part_b_fy_prep_stub_csv(&db, tenant_id, fy_start_year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India — Form 24Q salary payment month stub (CSV).** Annex-style **prep** for reconciliations —
    /// not TRACES **Form 24Q** upload; `gross` is a notional Section **192** salary base; TDS from payslip.
    async fn india_form24q_salary_payment_monthly_stub_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll Form 24Q stub export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_form24q_salary_payment_monthly_stub_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// **India — EPFO ECR-style monthly contribution prep (CSV).** UAN, capped EPF wage stub, EE/ER from
    /// payslip — not official Unified EPF **ECR** file format.
    async fn india_epf_monthly_ecr_prep_stub_csv(
        &self,
        ctx: &Context<'_>,
        month: i32,
        year: i32,
    ) -> Result<String> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll EPF ECR prep export requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_epf_monthly_ecr_prep_stub_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// `PENDING` payroll arrear accruals (oldest first by `createdAt` desc in service order). HR / statutory export role.
    async fn payroll_arrears(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<PayrollArrearDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "payroll arrear list requires payroll:statutory_export"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = arrear_service::list_pending_tenant(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(PayrollArrearDto::from).collect())
    }
}
