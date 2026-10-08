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

use crate::resolvers::query::parse_uuid;
use crate::resolvers::types::{
    AssignEmployeeSalaryStructureInput, CreatePayrollArrearInput, CreatePayrollCycleInput,
    EmployeeSalaryStructureDto, PayrollArrearDto, PayrollComplianceSettingDto, PayrollCycleDto,
    SalaryComponentDto, SalaryStructureComponentDto, SalaryStructureDto,
    UpsertPayrollComplianceSettingInput, UpsertSalaryComponentInput, UpsertSalaryStructureInput,
};
use crate::services::arrear_service;
use crate::services::payroll_service;

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
    async fn calculate_payroll_cycle(
        &self,
        ctx: &Context<'_>,
        cycle_id: ID,
        expected_revision: Option<i32>,
    ) -> Result<async_graphql::Json<crate::services::payroll_draft::PayrollDraft>> {
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        crate::services::payroll_draft::calculate_payroll_cycle(
            &db,
            tenant,
            actor,
            parse_uuid(&cycle_id, "cycleId")?,
            expected_revision,
        )
        .await
        .map(async_graphql::Json)
        .map_err(KabiPayError::into_graphql)
    }
    async fn finalize_payroll_cycle(
        &self,
        ctx: &Context<'_>,
        cycle_id: ID,
        draft_revision: i32,
        fingerprint: String,
        acknowledgement: async_graphql::Json<
            crate::services::payroll_draft::FinalizeAcknowledgement,
        >,
    ) -> Result<async_graphql::Json<crate::services::payroll_finalize::PayrollFinalization>> {
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        crate::services::payroll_finalize::finalize_payroll_cycle(
            &db,
            tenant,
            actor,
            parse_uuid(&cycle_id, "cycleId")?,
            draft_revision,
            &fingerprint,
            acknowledgement.0,
        )
        .await
        .map(async_graphql::Json)
        .map_err(KabiPayError::into_graphql)
    }
    async fn save_company_payroll_policy(
        &self,
        ctx: &Context<'_>,
        input: async_graphql::Json<crate::services::contribution_rules::ContributionPolicy>,
        expected_revision: Option<i32>,
    ) -> Result<async_graphql::Json<crate::services::contribution_policy_store::PolicyVersion>>
    {
        use sea_orm::TransactionTrait;
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        let txn = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let result = crate::services::contribution_policy_store::save(
            &txn,
            tenant,
            actor,
            input.0,
            expected_revision,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        txn.commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(async_graphql::Json(result))
    }
    async fn save_payroll_period_input(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        input: async_graphql::Json<serde_json::Value>,
        expected_revision: Option<i32>,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        use sea_orm::TransactionTrait;
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        let employee = parse_uuid(&employee_id, "employeeId")?;
        let input: crate::services::payroll_rules::PeriodInput = serde_json::from_value(input.0)
            .map_err(|_| {
                KabiPayError::Validation("invalid payroll period input".into()).into_graphql()
            })?;
        let transaction = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let saved = crate::services::payroll_period_input::save(
            &transaction,
            tenant,
            actor,
            employee,
            input,
            None,
            expected_revision,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let stored: crate::services::payroll_rules::PeriodInput =
            serde_json::from_value(saved.input.clone()).map_err(|_| {
                KabiPayError::Validation("stored payroll input is invalid".into()).into_graphql()
            })?;
        let prepared =
            crate::services::prepare_payroll::prepare(&transaction, tenant, employee, &stored)
                .await;
        let validation_error = match &prepared {
            Err(error) => Some(error.to_string()),
            Ok(value) => crate::services::imported_lwp::validate(
                &transaction,
                tenant,
                employee,
                &value.input,
                false,
            )
            .await
            .err()
            .map(|error| error.to_string()),
        };
        transaction
            .commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(async_graphql::Json(
            serde_json::json!({"id":saved.id,"input":saved.input,"revision":saved.revision,"ready":saved.ready,"validationError":validation_error,"calculation":prepared.ok()}),
        ))
    }
    async fn save_employee_payroll_eligibility(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        input: async_graphql::Json<crate::services::employee_eligibility::EligibilitySetting>,
    ) -> Result<async_graphql::Json<crate::services::employee_eligibility::EligibilitySetting>>
    {
        use sea_orm::TransactionTrait;
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        let transaction = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let result = crate::services::employee_eligibility::save(
            &transaction,
            tenant,
            parse_uuid(&employee_id, "employeeId")?,
            actor,
            input.0,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        transaction
            .commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(async_graphql::Json(result))
    }
    async fn set_salary_component_payslip_visibility(
        &self,
        ctx: &Context<'_>,
        component_id: ID,
        visible: bool,
    ) -> Result<bool> {
        require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        crate::services::component_display::save(
            &db,
            tenant,
            parse_uuid(&component_id, "componentId")?,
            visible,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }
    async fn save_payroll_unpaid_leave_policy(
        &self,
        ctx: &Context<'_>,
        input: super::types::SavePayrollUnpaidLeavePolicyInput,
    ) -> Result<super::types::PayrollUnpaidLeavePolicy> {
        let actor = require_payroll_manage_all(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let divisor = input
            .day_divisor
            .as_deref()
            .map(|v| {
                Decimal::from_str(v.trim()).map_err(|_| {
                    KabiPayError::Validation("day divisor must be a decimal number".into())
                        .into_graphql()
                })
            })
            .transpose()?;
        let db = tenant_db(ctx, tenant).await?;
        Ok(crate::services::unpaid_leave_policy::save(
            &db,
            tenant,
            actor,
            input.enabled,
            input.basic_component_code,
            divisor,
            input.treatment,
        )
        .await
        .map_err(KabiPayError::into_graphql)?
        .into())
    }
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
        if id.is_none() && m.r#type == "EMPLOYER_CONTRIBUTION" {
            crate::services::component_display::save(&db, tenant_id, m.id, false)
                .await
                .map_err(KabiPayError::into_graphql)?;
        }
        let visibility = crate::services::component_display::catalog(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let visible = visibility
            .get(&m.id)
            .copied()
            .unwrap_or(m.r#type != "EMPLOYER_CONTRIBUTION");
        let mut dto = SalaryComponentDto::from(m);
        dto.show_on_payslip = visible;
        Ok(dto)
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
                payroll_service::parse_money_decimal(
                    &component.calculation_value,
                    "calculationValue",
                )
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
                payroll_service::parse_money_decimal(
                    &override_input.calculation_value,
                    "calculationValue",
                )
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
        crate::services::payslip_template::resolve_payslip_template(None, input.payslip_template.as_deref())
            .map_err(KabiPayError::into_graphql)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let logo = input
            .payslip_logo_file_storage_id
            .as_ref()
            .map(|id| parse_uuid(id, "payslipLogoFileStorageId"))
            .transpose()?;
        let address = match input.payslip_company_address {
            async_graphql::MaybeUndefined::Undefined => None,
            async_graphql::MaybeUndefined::Null => Some(None),
            async_graphql::MaybeUndefined::Value(value) => Some(Some(value)),
        };
        let m = payroll_service::upsert_payroll_compliance_setting(
            &db,
            tenant_id,
            input.employer_tan,
            input.employer_legal_name,
            input.base_salary_component_code,
            input.arrear_salary_component_code,
            input.payslip_header_title,
            logo,
            input.payslip_template,
            input.payslip_employee_fields,
            address,
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
        let m = payroll_service::run_payroll_for_cycle(&db, tenant_id, cid, actor_user_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(PayrollCycleDto::from(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvers::query::QueryRoot;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::context::{
        ClientClaims, CLIENT_JWT_ISSUER, PERM_COMPENSATION_MANAGE, PERM_PAYROLL_MANAGE,
        PERM_PAYROLL_STATUTORY_EXPORT,
    };
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;

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
        assert_eq!(
            response.errors.len(),
            1,
            "unexpected response: {response:?}"
        );
        let message = &response.errors[0].message;
        assert!(
            message.contains(expected_message),
            "unexpected denial: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    fn assert_authorization_reached_db(response: &async_graphql::Response) {
        assert_eq!(
            response.errors.len(),
            1,
            "unexpected response: {response:?}"
        );
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

    fn protected_mutations() -> [&'static str; 11] {
        [
            r#"mutation { savePayrollUnpaidLeavePolicy(input: { enabled: false }) { enabled } }"#,
            r#"mutation { createPayrollArrear(input: { employeeId: "00000000-0000-0000-0000-000000000001", amount: "1" }) { id } }"#,
            r#"mutation { upsertSalaryComponent(input: { name: "Base", code: "BASIC", componentType: "EARNING", isTaxable: true, isFixed: true, isActive: true }) { id } }"#,
            r#"mutation { upsertSalaryStructure(input: { name: "Default", components: [] }) { id } }"#,
            r#"mutation { assignEmployeeSalaryStructure(input: { employeeId: "00000000-0000-0000-0000-000000000001", salaryStructureId: "00000000-0000-0000-0000-000000000002", annualCtc: "1", effectiveFrom: "2026-01-01", overrides: [] }) { id } }"#,
            r#"mutation { createPayrollCycle(input: { name: "January", month: 1, year: 2026 }) { id } }"#,
            r#"mutation { upsertPayrollComplianceSetting(input: {}) { employerTan } }"#,
            r#"mutation { runPayrollForCycle(payrollCycleId: "00000000-0000-0000-0000-000000000003") { id } }"#,
            r#"mutation { calculatePayrollCycle(cycleId: "00000000-0000-0000-0000-000000000003") }"#,
            r#"mutation { finalizePayrollCycle(cycleId: "00000000-0000-0000-0000-000000000003", draftRevision: 1, fingerprint: "reviewed", acknowledgement: {provisional_tax_employees: []}) }"#,
            r#"mutation { saveCompanyPayrollPolicy(input: {effective_from:"2026-10-01", lwp_divisor:31, origin:"HR_CONFIGURATION", reason:"Reviewed rules", pf_employee:{weights:{BASIC:"1"},rate:"0.12",rounding:"HALF_UP_2DP"},pf_employer:{weights:{BASIC:"1"},rate:"0.12",rounding:"HALF_UP_2DP"},esi_basis:{weights:{BASIC:"1"},rate:"0.0075",rounding:"CEIL_RUPEE"},esi_employer_rate:"0.0325",company_esi_covered:false,esi_mode:"CUSTOM_COMPONENTS",classifications:{},professional_tax:"200"}) }"#,
        ]
    }

    #[test]
    fn payroll_manage_requires_exact_all_scope() {
        assert!(
            payroll_manage_all_from_claims(Some(&claims(PERM_PAYROLL_MANAGE, Some("ALL")))).is_ok()
        );

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
    async fn payslip_template_write_requires_company_wide_payroll_management() {
        let mutation = r#"mutation { upsertPayrollComplianceSetting(input: { payslipTemplate: "TABLE" }) { payslipTemplate } }"#;
        for scope in [None, Some("SELF"), Some("TEAM")] {
            let response = execute_mutation(claims(PERM_PAYROLL_MANAGE, scope), mutation).await;
            assert_permission_denied_before_db(&response, PERM_PAYROLL_MANAGE);
        }
    }

    #[tokio::test]
    async fn payslip_template_write_accepts_the_company_admin_contract() {
        let response = execute_mutation(
            claims(PERM_PAYROLL_MANAGE, Some("ALL")),
            r#"mutation { upsertPayrollComplianceSetting(input: { payslipTemplate: "TABLE" }) { payslipTemplate } }"#,
        )
        .await;
        assert_authorization_reached_db(&response);
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
