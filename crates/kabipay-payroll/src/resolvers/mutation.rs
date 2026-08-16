//! Write operations: v1 pay run.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use rust_decimal::Decimal;
use std::str::FromStr;

use crate::resolvers::types::{
    AssignEmployeeSalaryStructureInput, EmployeeSalaryStructureDto,
    CreatePayrollArrearInput, CreatePayrollCycleInput, PayrollArrearDto, PayrollComplianceSettingDto,
    PayrollCycleDto, SalaryComponentDto, SalaryStructureComponentDto, SalaryStructureDto,
    UpsertPayrollComplianceSettingInput, UpsertSalaryComponentInput, UpsertSalaryStructureInput,
};
use crate::services::arrear_service;
use crate::services::payroll_service;
use crate::resolvers::query::parse_uuid;

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Record a **PENDING** arrear for an employee; amount is added on the next pay run (with an `ARREAR` line).
    async fn create_payroll_arrear(
        &self,
        ctx: &Context<'_>,
        input: CreatePayrollArrearInput,
    ) -> Result<PayrollArrearDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "create payroll arrear requires payroll:statutory_export or HR / tenant admin"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let eid = parse_uuid(&input.employee_id, "employeeId")?;
        let amount = Decimal::from_str(&input.amount.trim())
            .map_err(|e| KabiPayError::Validation(format!("amount: {e}")).into_graphql())?;
        let m = arrear_service::create_arrear(&db, tenant_id, eid, amount, input.reason)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollArrearDto::from(m))
    }

    async fn upsert_salary_component(
        &self,
        ctx: &Context<'_>,
        input: UpsertSalaryComponentInput,
    ) -> Result<SalaryComponentDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() && !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden(
                    "upsert salary component requires payroll:statutory_export or compensation:manage".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = input
            .id
            .as_ref()
            .map(|id| parse_uuid(id, "id"))
            .transpose()?;
        let m = payroll_service::upsert_salary_component(
            &db,
            tenant_id,
            id,
            input.name,
            input.code,
            input.component_type,
            input.is_taxable,
            input.is_fixed,
            input.is_active,
            input.formula_expression,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(SalaryComponentDto::from(m))
    }

    async fn upsert_salary_structure(
        &self,
        ctx: &Context<'_>,
        input: UpsertSalaryStructureInput,
    ) -> Result<SalaryStructureDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() && !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden(
                    "upsert salary structure requires payroll:statutory_export or compensation:manage".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = input
            .id
            .as_ref()
            .map(|id| parse_uuid(id, "id"))
            .transpose()?;
        let mut components = Vec::with_capacity(input.components.len());
        for component in input.components {
            components.push((
                parse_uuid(&component.salary_component_id, "salaryComponentId")?,
                component.calculation_basis,
                payroll_service::parse_money_decimal(&component.calculation_value, "calculationValue")
                    .map_err(KabiPayError::into_graphql)?,
                component.display_order,
            ));
        }
        let (structure, component_rows) = payroll_service::upsert_salary_structure(
            &db,
            tenant_id,
            id,
            input.name,
            input.description,
            components,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(SalaryStructureDto::from_head(
            structure,
            component_rows
                .into_iter()
                .map(|(row, component)| SalaryStructureComponentDto::from_parts(row, component))
                .collect(),
        ))
    }

    async fn assign_employee_salary_structure(
        &self,
        ctx: &Context<'_>,
        input: AssignEmployeeSalaryStructureInput,
    ) -> Result<EmployeeSalaryStructureDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() && !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden(
                    "assign employee salary structure requires payroll:statutory_export or compensation:manage".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let salary_structure_id = parse_uuid(&input.salary_structure_id, "salaryStructureId")?;
        let annual_ctc = payroll_service::parse_money_decimal(&input.annual_ctc, "annualCtc")
            .map_err(KabiPayError::into_graphql)?;
        let mut overrides = Vec::with_capacity(input.overrides.len());
        for override_input in input.overrides {
            overrides.push((
                parse_uuid(&override_input.salary_component_id, "salaryComponentId")?,
                override_input.calculation_basis,
                payroll_service::parse_money_decimal(&override_input.calculation_value, "calculationValue")
                    .map_err(KabiPayError::into_graphql)?,
                override_input.notes,
                override_input.is_active,
            ));
        }
        let row = payroll_service::assign_employee_salary_structure(
            &db,
            tenant_id,
            employee_id,
            salary_structure_id,
            annual_ctc,
            input.effective_from,
            input.effective_to,
            overrides,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(EmployeeSalaryStructureDto::from(row))
    }

    /// Create a **DRAFT** payroll cycle for a calendar month/year (one per tenant per period in v1).
    /// Same RBAC as **run payroll** (statutory export / HR / tenant admin).
    async fn create_payroll_cycle(
        &self,
        ctx: &Context<'_>,
        input: CreatePayrollCycleInput,
    ) -> Result<PayrollCycleDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "create payroll cycle requires payroll:statutory_export or HR / tenant admin role"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let m = payroll_service::create_payroll_cycle(
            &db,
            tenant_id,
            input.name,
            input.month,
            input.year,
            input.payment_date,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollCycleDto::from(m))
    }

    /// Upsert tenant employer TAN and legal name for India statutory payroll CSV placeholders.
    async fn upsert_payroll_compliance_setting(
        &self,
        ctx: &Context<'_>,
        input: UpsertPayrollComplianceSettingInput,
    ) -> Result<PayrollComplianceSettingDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "upsert payroll compliance setting requires payroll:statutory_export or HR / tenant admin role"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let logo = input
            .payslip_logo_file_storage_id
            .as_ref()
            .map(|id| parse_uuid(id, "payslipLogoFileStorageId"))
            .transpose()?;
        let m = payroll_service::upsert_payroll_compliance_setting(
            &db,
            tenant_id,
            input.employer_tan,
            input.employer_legal_name,
            input.base_salary_component_code,
            input.arrear_salary_component_code,
            input.payslip_header_title,
            logo,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollComplianceSettingDto::from(m))
    }

    /// **Pay run (v2)** — generate missing payslips for a `DRAFT` cycle, then set the cycle to
    /// `PROCESSED`. Per employee: latest `employment_history.salary` as BASIC, PENDING
    /// `payroll_arrear` as an `ARREAR` `salary_component` line, India statutory stub and TDS from
    /// `tax_computation` for the pay month’s India FY. Same RBAC as India statutory CSV export.
    async fn run_payroll_for_cycle(
        &self,
        ctx: &Context<'_>,
        payroll_cycle_id: ID,
    ) -> Result<PayrollCycleDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_export_payroll_statutory() {
            return Err(
                KabiPayError::Forbidden(
                    "run payroll requires payroll:statutory_export or HR / tenant admin role"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&payroll_cycle_id, "payrollCycleId")?;
        let m = payroll_service::run_payroll_for_cycle(
            &db,
            tenant_id,
            cid,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollCycleDto::from(m))
    }
}
