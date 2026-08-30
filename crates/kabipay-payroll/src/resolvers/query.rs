//! Root query resolvers for kabipay-payroll.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_claims, resolve_employee_scope_filter, resolve_viewer_employee,
        EmployeeScopeFilter,
    },
    context::{
        ClientClaims, ScopeType, PERM_PAYROLL_MANAGE, PERM_PAYROLL_READ,
        PERM_PAYROLL_STATUTORY_EXPORT,
    },
    file_download_token::{file_download_claims, public_employee_file_download_url},
    subgraph::{require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError, KabiPayResult,
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

fn payroll_tenant_all_scope_from_claims(
    claims: Option<&ClientClaims>,
    permission: &'static str,
) -> KabiPayResult<ScopeType> {
    let scope = data_scope_from_claims(claims, permission)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires ALL scope"
        )));
    }
    Ok(scope)
}

fn require_payroll_tenant_all_scope(ctx: &Context<'_>, permission: &'static str) -> Result<()> {
    payroll_tenant_all_scope_from_claims(ctx.data_opt::<ClientClaims>(), permission)
        .map(|_| ())
        .map_err(KabiPayError::into_graphql)
}

fn payroll_read_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    data_scope_from_claims(claims, PERM_PAYROLL_READ)
}

fn payroll_read_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    payroll_read_scope_from_claims(ctx.data_opt::<ClientClaims>())
        .map_err(KabiPayError::into_graphql)
}

fn require_payroll_target_scope(
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> KabiPayResult<()> {
    if filter.allows_employee(target_employee_id) {
        return Ok(());
    }
    Err(KabiPayError::Forbidden(
        "payroll:read scope does not include target employee".into(),
    ))
}

async fn load_scoped_employee_target_with<
    T,
    ResolveJwtEmployee,
    ResolveJwtEmployeeFuture,
    ResolveViewer,
    ResolveViewerFuture,
    ResolveScope,
    ResolveScopeFuture,
    Load,
    LoadFuture,
>(
    requested_employee_id: Option<Uuid>,
    scope: ScopeType,
    resolve_jwt_employee: ResolveJwtEmployee,
    resolve_viewer: ResolveViewer,
    resolve_scope: ResolveScope,
    load: Load,
) -> Result<T>
where
    ResolveJwtEmployee: FnOnce() -> ResolveJwtEmployeeFuture,
    ResolveJwtEmployeeFuture: std::future::Future<Output = Result<Uuid>>,
    ResolveViewer: FnOnce() -> ResolveViewerFuture,
    ResolveViewerFuture: std::future::Future<
        Output = Result<Option<kabipay_common::context::ClientViewerEmployee>>,
    >,
    ResolveScope: FnOnce(
        ScopeType,
        Option<kabipay_common::context::ClientViewerEmployee>,
    ) -> ResolveScopeFuture,
    ResolveScopeFuture: std::future::Future<Output = Result<EmployeeScopeFilter>>,
    Load: FnOnce(Uuid) -> LoadFuture,
    LoadFuture: std::future::Future<Output = Result<T>>,
{
    let target_employee_id = match requested_employee_id {
        Some(employee_id) => employee_id,
        None => resolve_jwt_employee().await?,
    };
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer().await?
    };
    let filter = resolve_scope(scope, viewer).await?;
    require_payroll_target_scope(&filter, target_employee_id)
        .map_err(KabiPayError::into_graphql)?;
    load(target_employee_id).await
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_MANAGE)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_MANAGE)?;
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
        let scope = payroll_read_scope(ctx)?;
        let eid = parse_uuid(&employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let as_of = as_of.unwrap_or_else(|| chrono::Utc::now().date_naive());
        let jwt_db = &db;
        let viewer_db = &db;
        let scope_db = &db;
        let load_db = &db;
        let preview = load_scoped_employee_target_with(
            Some(eid),
            scope,
            || async move {
                resolve_client_employee_id(ctx, jwt_db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            || async move { resolve_viewer_employee(ctx, viewer_db, tenant_id).await },
            |resolved_scope, viewer| async move {
                resolve_employee_scope_filter(scope_db, tenant_id, resolved_scope, viewer)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            |target_employee_id| async move {
                payroll_service::preview_employee_salary_breakup(
                    load_db,
                    tenant_id,
                    target_employee_id,
                    as_of,
                )
                .await
                .map_err(KabiPayError::into_graphql)
            },
        )
        .await?;
        let Some(preview) = preview
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_MANAGE)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = payroll_service::list_cycles(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(PayrollCycleDto::from).collect())
    }

    /// Employer TAN, payslip branding, component codes (optional row per tenant).
    /// Requires `payroll:read` so employees can render branded payslips;
    /// `upsertPayrollComplianceSetting` remains a separately authorized mutation.
    async fn payroll_compliance_setting(&self, ctx: &Context<'_>) -> Result<Option<PayrollComplianceSettingDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        payroll_read_scope(ctx)?;
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
        let tenant_id = require_tenant_id(ctx)?;
        payroll_read_scope(ctx)?;
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
        let scope = payroll_read_scope(ctx)?;
        let sid = parse_uuid(&id, "id")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let row = payroll_service::find_scoped_payslip_detail(&db, tenant_id, sid, &filt)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some((p, c)) = row else {
            return Ok(None);
        };
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
        let scope = payroll_read_scope(ctx)?;
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        let jwt_db = &db;
        let viewer_db = &db;
        let scope_db = &db;
        let load_db = &db;
        load_scoped_employee_target_with(
            requested_employee_id,
            scope,
            || async move {
                resolve_client_employee_id(ctx, jwt_db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            || async move { resolve_viewer_employee(ctx, viewer_db, tenant_id).await },
            |resolved_scope, viewer| async move {
                resolve_employee_scope_filter(scope_db, tenant_id, resolved_scope, viewer)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            |target_employee_id| async move {
                let list = payroll_service::list_payslips(
                    load_db,
                    tenant_id,
                    Some(target_employee_id),
                    limit,
                )
                .await
                .map_err(KabiPayError::into_graphql)?;
                let ids: Vec<Uuid> = list.iter().map(|p| p.id).collect();
                let lines = payroll_service::payslip_lines_by_payslip_ids(load_db, tenant_id, &ids)
                    .await
                    .map_err(KabiPayError::into_graphql)?;
                Ok(list
                    .into_iter()
                    .map(|p| {
                        let c = lines.get(&p.id).cloned().unwrap_or_default();
                        PayslipDetailDto::from_head(p, c)
                    })
                    .collect())
            },
        )
        .await
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
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
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_STATUTORY_EXPORT)?;
        let db = tenant_db(ctx, tenant_id).await?;
        payroll_service::india_epf_monthly_ecr_prep_stub_csv(&db, tenant_id, month, year)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// `PENDING` payroll arrear accruals (oldest first by `createdAt` desc in service order).
    /// Requires exact `payroll:manage` permission.
    async fn payroll_arrears(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<PayrollArrearDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        require_payroll_tenant_all_scope(ctx, PERM_PAYROLL_MANAGE)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = arrear_service::list_pending_tenant(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(PayrollArrearDto::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use kabipay_common::client_data_scope::{
        resolve_employee_scope_filter_with_connection, EmployeeScopeFilter,
    };
    use kabipay_common::context::{
        ClientClaims, ScopeType, CLIENT_JWT_ISSUER, EMPLOYMENT_STATUS_ACTIVE,
        EMPLOYMENT_STATUS_PROBATION, PERM_EMPLOYEE_READ, PERM_PAYROLL_MANAGE,
        PERM_PAYROLL_READ, PERM_PAYROLL_STATUTORY_EXPORT,
    };
    use kabipay_common::subgraph::TenantId;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
        ProxyRow, Statement,
    };
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
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
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    fn claims_without_permissions() -> ClientClaims {
        let mut claims = claims("unrelated:read", Some("ALL"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        claims
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

    fn assert_permission_denied_before_db(
        response: &async_graphql::Response,
        expected_message: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(expected_message),
            "unexpected denial: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TargetBoundaryOperation {
        ResolveJwtEmployee,
        ResolveViewer,
        ResolveScope(ScopeType, Option<Uuid>),
        Load(Uuid),
    }

    #[derive(Debug)]
    struct ScopeProxy {
        rows: Vec<ProxyRow>,
        statements: Arc<Mutex<Vec<Statement>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ScopeProxy {
        async fn query(&self, statement: Statement) -> std::result::Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("scope statement recorder lock")
                .push(statement);
            Ok(self.rows.clone())
        }

        async fn execute(
            &self,
            _statement: Statement,
        ) -> std::result::Result<ProxyExecResult, DbErr> {
            Err(DbErr::Custom(
                "scope resolver unexpectedly executed a write statement".into(),
            ))
        }
    }

    struct TargetBoundaryFixture {
        tenant_id: Uuid,
        jwt_employee_id: Option<Uuid>,
        viewer: Option<kabipay_common::context::ClientViewerEmployee>,
        scope_db: DatabaseConnection,
        scope_statements: Arc<Mutex<Vec<Statement>>>,
        operations: RefCell<Vec<TargetBoundaryOperation>>,
    }

    impl TargetBoundaryFixture {
        async fn new(
            jwt_employee_id: Option<Uuid>,
            viewer: Option<kabipay_common::context::ClientViewerEmployee>,
            team_employee_ids: Vec<Uuid>,
        ) -> Self {
            let rows: Vec<ProxyRow> = team_employee_ids
                .into_iter()
                .map(|employee_id| {
                    ProxyRow::new(BTreeMap::from([("id".into(), employee_id.into())]))
                })
                .collect();
            let scope_statements = Arc::new(Mutex::new(Vec::new()));
            let scope_db = Database::connect_proxy(
                DbBackend::Postgres,
                Arc::new(Box::new(ScopeProxy {
                    rows,
                    statements: Arc::clone(&scope_statements),
                })),
            )
            .await
            .expect("PostgreSQL scope proxy connection");
            Self {
                tenant_id: Uuid::new_v4(),
                jwt_employee_id,
                viewer,
                scope_db,
                scope_statements,
                operations: RefCell::new(Vec::new()),
            }
        }

        fn operations(&self) -> Vec<TargetBoundaryOperation> {
            self.operations.borrow().clone()
        }

        async fn resolve_scope(
            &self,
            scope: ScopeType,
            viewer: Option<kabipay_common::context::ClientViewerEmployee>,
        ) -> Result<EmployeeScopeFilter> {
            self.operations.borrow_mut().push(
                TargetBoundaryOperation::ResolveScope(scope, viewer.map(|v| v.employee_id)),
            );
            resolve_employee_scope_filter_with_connection(
                &self.scope_db,
                self.tenant_id,
                scope,
                viewer,
            )
            .await
            .map_err(KabiPayError::into_graphql)
        }

        fn scope_statements(&self) -> Vec<Statement> {
            self.scope_statements
                .lock()
                .expect("scope statement recorder lock")
                .clone()
        }

        async fn execute(
            &self,
            requested_employee_id: Option<Uuid>,
            scope: ScopeType,
        ) -> Result<Uuid> {
            load_scoped_employee_target_with(
                requested_employee_id,
                scope,
                || async {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::ResolveJwtEmployee);
                    self.jwt_employee_id.ok_or_else(|| {
                        KabiPayError::Forbidden("JWT-bound employee required".into()).into_graphql()
                    })
                },
                || async {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::ResolveViewer);
                    Ok(self.viewer)
                },
                |scope, viewer| async move { self.resolve_scope(scope, viewer).await },
                |target_employee_id| async move {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::Load(target_employee_id));
                    Ok(target_employee_id)
                },
            )
            .await
        }
    }

    #[tokio::test]
    async fn payroll_boundary_explicit_all_succeeds_without_linked_employee() {
        let target_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

        assert_eq!(
            fixture
                .execute(Some(target_id), ScopeType::All)
                .await
                .expect("explicit ALL target reaches payroll service"),
            target_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveScope(ScopeType::All, None),
                TargetBoundaryOperation::Load(target_id),
            ]
        );
    }

    #[tokio::test]
    async fn payroll_boundary_omitted_target_requires_jwt_employee() {
        let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

        let error = fixture
            .execute(None, ScopeType::All)
            .await
            .expect_err("omitted target must require JWT employee binding");

        assert!(error.message.contains("JWT-bound employee required"));
        assert_eq!(
            fixture.operations(),
            vec![TargetBoundaryOperation::ResolveJwtEmployee]
        );
    }

    #[tokio::test]
    async fn payroll_boundary_omitted_target_uses_jwt_employee_then_loads() {
        let jwt_employee_id = Uuid::new_v4();
        let fixture =
            TargetBoundaryFixture::new(Some(jwt_employee_id), None, Vec::new()).await;

        assert_eq!(
            fixture
                .execute(None, ScopeType::All)
                .await
                .expect("JWT-bound employee reaches payroll service"),
            jwt_employee_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveJwtEmployee,
                TargetBoundaryOperation::ResolveScope(ScopeType::All, None),
                TargetBoundaryOperation::Load(jwt_employee_id),
            ]
        );
    }

    #[tokio::test]
    async fn payroll_boundary_self_and_team_without_viewer_deny_before_service() {
        for scope in [ScopeType::Self_, ScopeType::Team] {
            let target_id = Uuid::new_v4();
            let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

            let error = fixture
                .execute(Some(target_id), scope)
                .await
                .expect_err("viewer-bound scope must deny without a viewer");

            assert!(error.message.contains("scope does not include target employee"));
            assert_eq!(
                fixture.operations(),
                vec![
                    TargetBoundaryOperation::ResolveViewer,
                    TargetBoundaryOperation::ResolveScope(scope, None),
                ]
            );
        }
    }

    #[tokio::test]
    async fn payroll_boundary_team_uses_recursive_reporting_hierarchy() {
        let manager_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(
            None,
            Some(kabipay_common::context::ClientViewerEmployee {
                employee_id: manager_id,
                department_id: None,
            }),
            vec![manager_id, direct_report_id, descendant_id],
        )
        .await;
        let tenant_id = fixture.tenant_id;

        assert_eq!(
            fixture
                .execute(Some(descendant_id), ScopeType::Team)
                .await
                .expect("recursive TEAM descendant reaches payroll service"),
            descendant_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveViewer,
                TargetBoundaryOperation::ResolveScope(ScopeType::Team, Some(manager_id)),
                TargetBoundaryOperation::Load(descendant_id),
            ]
        );
        let statements = fixture.scope_statements();
        assert_eq!(statements.len(), 1);
        let statement = &statements[0];
        assert_eq!(statement.db_backend, DbBackend::Postgres);
        assert!(statement.sql.contains("WITH RECURSIVE team"));
        assert!(statement.sql.contains("root.tenant_id = $1"));
        assert!(statement.sql.contains("child.tenant_id = $1"));
        assert_eq!(
            statement.values.as_ref().expect("bound TEAM query values").0,
            vec![
                tenant_id.into(),
                manager_id.into(),
                EMPLOYMENT_STATUS_ACTIVE.into(),
                EMPLOYMENT_STATUS_PROBATION.into(),
            ]
        );
    }

    #[tokio::test]
    async fn payroll_boundary_out_of_scope_target_skips_service() {
        let manager_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(
            None,
            Some(kabipay_common::context::ClientViewerEmployee {
                employee_id: manager_id,
                department_id: None,
            }),
            vec![manager_id, direct_report_id],
        )
        .await;
        let tenant_id = fixture.tenant_id;

        fixture
            .execute(Some(outside_id), ScopeType::Team)
            .await
            .expect_err("outside target must be rejected");

        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveViewer,
                TargetBoundaryOperation::ResolveScope(ScopeType::Team, Some(manager_id)),
            ]
        );
        let statements = fixture.scope_statements();
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0]
                .values
                .as_ref()
                .expect("bound TEAM query values")
                .0,
            vec![
                tenant_id.into(),
                manager_id.into(),
                EMPLOYMENT_STATUS_ACTIVE.into(),
                EMPLOYMENT_STATUS_PROBATION.into(),
            ]
        );
    }

    #[test]
    fn payroll_read_gate_requires_its_own_valid_exact_scope() {
        for (wire_scope, expected) in [
            ("SELF", ScopeType::Self_),
            ("TEAM", ScopeType::Team),
            ("DEPARTMENT", ScopeType::Department),
            ("ALL", ScopeType::All),
        ] {
            assert_eq!(
                payroll_read_scope_from_claims(Some(&claims(
                    PERM_PAYROLL_READ,
                    Some(wire_scope),
                )))
                .expect("valid exact payroll read scope"),
                expected
            );
        }

        for denied_claims in [
            claims(PERM_PAYROLL_READ, None),
            claims(PERM_PAYROLL_READ, Some("INVALID")),
            claims(PERM_PAYROLL_MANAGE, Some("ALL")),
            claims(PERM_PAYROLL_STATUTORY_EXPORT, Some("ALL")),
        ] {
            assert!(matches!(
                payroll_read_scope_from_claims(Some(&denied_claims)),
                Err(KabiPayError::Forbidden(_))
            ));
        }
    }

    #[test]
    fn tenant_wide_payroll_permissions_require_exact_all_scope() {
        for permission in [PERM_PAYROLL_MANAGE, PERM_PAYROLL_STATUTORY_EXPORT] {
            assert!(payroll_tenant_all_scope_from_claims(Some(&claims(
                permission,
                Some("ALL"),
            )), permission)
            .is_ok());

            for scope in [
                None,
                Some("INVALID"),
                Some("SELF"),
                Some("TEAM"),
                Some("DEPARTMENT"),
            ] {
                assert!(matches!(
                    payroll_tenant_all_scope_from_claims(
                        Some(&claims(permission, scope)),
                        permission,
                    ),
                    Err(KabiPayError::Forbidden(_))
                ));
            }
        }

        assert!(matches!(
            payroll_tenant_all_scope_from_claims(
                Some(&claims(PERM_PAYROLL_READ, Some("ALL"))),
                PERM_PAYROLL_MANAGE,
            ),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[test]
    fn payroll_target_scope_enforces_self_team_all_and_empty_filters() {
        let viewer_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let self_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id]);
        let team_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]);

        assert!(require_payroll_target_scope(&self_filter, viewer_id).is_ok());
        assert!(matches!(
            require_payroll_target_scope(&self_filter, descendant_id),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(require_payroll_target_scope(&team_filter, descendant_id).is_ok());
        assert!(matches!(
            require_payroll_target_scope(&team_filter, outside_id),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(
            require_payroll_target_scope(&EmployeeScopeFilter::Unrestricted, outside_id).is_ok()
        );
        assert!(matches!(
            require_payroll_target_scope(&EmployeeScopeFilter::Empty, outside_id),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn explicit_payroll_target_allows_all_without_viewer_and_denies_self_or_team() {
        let tenant_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        let all = resolve_employee_scope_filter(
            &DatabaseConnection::Disconnected,
            tenant_id,
            ScopeType::All,
            None,
        )
        .await
        .expect("ALL scope does not need a viewer");
        assert!(require_payroll_target_scope(&all, target_id).is_ok());

        for scope in [ScopeType::Self_, ScopeType::Team] {
            let filter = resolve_employee_scope_filter(
                &DatabaseConnection::Disconnected,
                tenant_id,
                scope,
                None,
            )
            .await
            .expect("missing viewer resolves to an empty target scope");
            assert!(matches!(filter, EmployeeScopeFilter::Empty));
            assert!(matches!(
                require_payroll_target_scope(&filter, target_id),
                Err(KabiPayError::Forbidden(_))
            ));
        }
    }

    #[tokio::test]
    async fn every_protected_payroll_query_uses_its_exact_permission_before_db_access() {
        let employee_id = Uuid::new_v4();
        let record_id = Uuid::new_v4();
        let fields = vec![
            (
                "{ salaryComponents { __typename } }".to_string(),
                PERM_PAYROLL_MANAGE,
                PERM_PAYROLL_READ,
                true,
            ),
            (
                "{ salaryStructures { __typename } }".to_string(),
                PERM_PAYROLL_MANAGE,
                PERM_PAYROLL_STATUTORY_EXPORT,
                true,
            ),
            (
                format!(
                    "{{ employeeSalaryBreakupPreview(employeeId: \"{employee_id}\") {{ __typename }} }}"
                ),
                PERM_PAYROLL_READ,
                PERM_PAYROLL_MANAGE,
                false,
            ),
            (
                "{ payrollCycles { __typename } }".to_string(),
                PERM_PAYROLL_MANAGE,
                PERM_PAYROLL_READ,
                true,
            ),
            (
                "{ payrollComplianceSetting { __typename } }".to_string(),
                PERM_PAYROLL_READ,
                PERM_PAYROLL_MANAGE,
                false,
            ),
            (
                format!("{{ payslipLogoSignedReadUrl(fileStorageId: \"{record_id}\") }}"),
                PERM_PAYROLL_READ,
                PERM_PAYROLL_MANAGE,
                false,
            ),
            (
                format!("{{ payslip(id: \"{record_id}\") {{ __typename }} }}"),
                PERM_PAYROLL_READ,
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                format!("{{ payslips(employeeId: \"{employee_id}\") {{ __typename }} }}"),
                PERM_PAYROLL_READ,
                PERM_EMPLOYEE_READ,
                false,
            ),
            (
                "{ indiaTdsMonthlySummaryCsv(month: 8, year: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaPfEsiMonthlySummaryCsv(month: 8, year: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ payrollBankTransferCsv(month: 8, year: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ payrollIndiaBulkNeftCreditCsv(month: 8, year: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaFyPayrollEmployeeTotalsCsv(fyStartYear: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaFyQuarterPayrollEmployeeTotalsCsv(fyStartYear: 2026, quarter: 1) }"
                    .to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaForm16PartBFyPrepStubCsv(fyStartYear: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaForm24QSalaryPaymentMonthlyStubCsv(month: 8, year: 2026) }"
                    .to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ indiaEpfMonthlyEcrPrepStubCsv(month: 8, year: 2026) }".to_string(),
                PERM_PAYROLL_STATUTORY_EXPORT,
                PERM_PAYROLL_MANAGE,
                true,
            ),
            (
                "{ payrollArrears { __typename } }".to_string(),
                PERM_PAYROLL_MANAGE,
                PERM_PAYROLL_STATUTORY_EXPORT,
                true,
            ),
        ];

        for (query, required_permission, sibling_permission, requires_all_scope) in fields {
            for denied_claims in [
                claims_without_permissions(),
                claims(sibling_permission, Some("ALL")),
            ] {
                let response = execute_query(denied_claims, &query).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!("{required_permission} permission required"),
                );
            }

            for scope in [None, Some("INVALID")] {
                let response = execute_query(claims(required_permission, scope), &query).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!(
                        "{required_permission} permission requires an explicit valid scope"
                    ),
                );
            }

            if requires_all_scope {
                for scope in ["SELF", "TEAM", "DEPARTMENT"] {
                    let response =
                        execute_query(claims(required_permission, Some(scope)), &query).await;
                    assert_permission_denied_before_db(
                        &response,
                        &format!("{required_permission} permission requires ALL scope"),
                    );
                }
            }
        }
    }
}
