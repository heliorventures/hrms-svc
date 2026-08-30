//! Write operations: v1 pay run.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::data_scope_from_claims,
    context::{ClientClaims, ScopeType, PERM_PAYROLL_MANAGE},
    subgraph::{require_tenant_id, tenant_db},
    KabiPayError, KabiPayResult,
};

use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

use crate::resolvers::types::{
    AssignEmployeeSalaryStructureInput, EmployeeSalaryStructureDto,
    CreatePayrollArrearInput, CreatePayrollCycleInput, PayrollArrearDto, PayrollComplianceSettingDto,
    PayrollCycleDto, SalaryComponentDto, SalaryStructureComponentDto, SalaryStructureDto,
    UpsertPayrollComplianceSettingInput, UpsertSalaryComponentInput, UpsertSalaryStructureInput,
};
use crate::services::arrear_service;
use crate::services::payroll_service;
use crate::resolvers::query::parse_uuid;

fn payroll_manage_all_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<()> {
    let scope = data_scope_from_claims(claims, PERM_PAYROLL_MANAGE)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden(format!(
            "{PERM_PAYROLL_MANAGE} permission requires ALL scope"
        )));
    }
    Ok(())
}

fn require_payroll_manage_all(ctx: &Context<'_>) -> Result<Uuid> {
    payroll_manage_all_from_claims(ctx.data_opt::<ClientClaims>())
        .map_err(KabiPayError::into_graphql)?;
    Ok(ctx
        .data::<ClientClaims>()
        .map_err(|_| KabiPayError::Unauthorised.into_graphql())?
        .sub)
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Record a **PENDING** arrear for an employee; amount is added on the next pay run (with an `ARREAR` line).
    async fn create_payroll_arrear(
        &self,
        ctx: &Context<'_>,
        input: CreatePayrollArrearInput,
    ) -> Result<PayrollArrearDto> {
        require_payroll_manage_all(ctx)?;
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
        require_payroll_manage_all(ctx)?;
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
        require_payroll_manage_all(ctx)?;
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
        require_payroll_manage_all(ctx)?;
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
    /// Same authorization as **run payroll**: `payroll:manage` with `ALL` scope.
    async fn create_payroll_cycle(
        &self,
        ctx: &Context<'_>,
        input: CreatePayrollCycleInput,
    ) -> Result<PayrollCycleDto> {
        require_payroll_manage_all(ctx)?;
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
        require_payroll_manage_all(ctx)?;
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
        let actor_user_id = require_payroll_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&payroll_cycle_id, "payrollCycleId")?;
        let m = payroll_service::run_payroll_for_cycle(
            &db,
            tenant_id,
            cid,
            actor_user_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollCycleDto::from(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::context::{
        ClientClaims, CLIENT_JWT_ISSUER, PERM_COMPENSATION_MANAGE, PERM_PAYROLL_MANAGE,
        PERM_PAYROLL_STATUTORY_EXPORT,
    };
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;
    use crate::resolvers::query::QueryRoot;

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
            employee_id: None,
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute_mutation(claims: ClientClaims, mutation: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, MutationRoot, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(mutation))
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

    fn assert_authorization_reached_db(response: &async_graphql::Response) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let code = response.errors[0]
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("code"))
            .cloned();
        assert_eq!(
            code,
            Some(async_graphql::Value::from("INTERNAL_ERROR")),
            "exact payroll:manage ALL authority did not reach the database boundary: {response:?}"
        );
    }

    fn protected_mutations() -> [&'static str; 7] {
        [
            r#"mutation { createPayrollArrear(input: { employeeId: "00000000-0000-0000-0000-000000000001", amount: "1" }) { id } }"#,
            r#"mutation { upsertSalaryComponent(input: { name: "Base", code: "BASIC", componentType: "EARNING", isTaxable: true, isFixed: true, isActive: true }) { id } }"#,
            r#"mutation { upsertSalaryStructure(input: { name: "Default", components: [] }) { id } }"#,
            r#"mutation { assignEmployeeSalaryStructure(input: { employeeId: "00000000-0000-0000-0000-000000000001", salaryStructureId: "00000000-0000-0000-0000-000000000002", annualCtc: "1", effectiveFrom: "2026-01-01", overrides: [] }) { id } }"#,
            r#"mutation { createPayrollCycle(input: { name: "January", month: 1, year: 2026 }) { id } }"#,
            r#"mutation { upsertPayrollComplianceSetting(input: {}) { employerTan } }"#,
            r#"mutation { runPayrollForCycle(payrollCycleId: "00000000-0000-0000-0000-000000000003") { id } }"#,
        ]
    }

    #[test]
    fn payroll_manage_requires_exact_all_scope() {
        assert!(payroll_manage_all_from_claims(Some(&claims(
            PERM_PAYROLL_MANAGE,
            Some("ALL")
        )))
        .is_ok());

        for denied in [
            claims(PERM_PAYROLL_MANAGE, None),
            claims(PERM_PAYROLL_MANAGE, Some("INVALID")),
            claims(PERM_PAYROLL_MANAGE, Some("SELF")),
            claims(PERM_PAYROLL_MANAGE, Some("TEAM")),
            claims(PERM_PAYROLL_STATUTORY_EXPORT, Some("ALL")),
        ] {
            assert!(payroll_manage_all_from_claims(Some(&denied)).is_err());
        }
        assert!(payroll_manage_all_from_claims(None).is_err());
    }

    #[tokio::test]
    async fn every_payroll_mutation_rejects_sibling_permission_before_database_access() {
        for sibling_permission in [PERM_PAYROLL_STATUTORY_EXPORT, PERM_COMPENSATION_MANAGE] {
            for mutation in protected_mutations() {
                let response =
                    execute_mutation(claims(sibling_permission, Some("ALL")), mutation).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!("{PERM_PAYROLL_MANAGE} permission required"),
                );
            }
        }
    }

    #[tokio::test]
    async fn every_payroll_mutation_rejects_non_all_scope_before_database_access() {
        for denied_scope in [None, Some("SELF"), Some("TEAM"), Some("DEPARTMENT")] {
            for mutation in protected_mutations() {
                let response =
                    execute_mutation(claims(PERM_PAYROLL_MANAGE, denied_scope), mutation).await;
                assert_permission_denied_before_db(&response, PERM_PAYROLL_MANAGE);
            }
        }
    }

    #[tokio::test]
    async fn every_payroll_mutation_with_exact_all_scope_reaches_database_boundary() {
        for mutation in protected_mutations() {
            let response =
                execute_mutation(claims(PERM_PAYROLL_MANAGE, Some("ALL")), mutation).await;
            assert_authorization_reached_db(&response);
        }
    }
}
