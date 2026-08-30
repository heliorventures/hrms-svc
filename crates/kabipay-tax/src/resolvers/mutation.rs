//! Write operations for employee tax computations / declarations.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::data_scope_from_claims,
    context::{
        ClientClaims, ScopeType, PERM_TAX_APPROVE, PERM_TAX_MANAGE, PERM_TAX_SUBMIT,
    },
    subgraph::{require_tenant_id, tenant_db},
    KabiPayError, KabiPayResult,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

use crate::resolvers::types::{
    SubmitTaxProofLineInput, TaxComputationDto, TaxProofLineDto, TaxSectionDefinitionDto,
    TaxSlabDto, TaxConfigurationVersionDto, UpsertTaxComputationInput,
    UpsertTaxConfigurationVersionInput, UpsertTaxSectionDefinitionInput, UpsertTaxSlabInput,
};
use crate::services::tax_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Create or update the `tax_computation` row for this employee, config version, and year.
    ///
    /// **Note:** `totalDeductions` may be **overwritten** when tax proof lines are approved
    /// (see `submitTaxProofLine` / `approveTaxProofLine`); use `taxProofLines` + approved
    /// workflow for year-end truth.
    async fn upsert_tax_computation(
        &self,
        ctx: &Context<'_>,
        input: UpsertTaxComputationInput,
    ) -> Result<TaxComputationDto> {
        let employee_id = tax_submit_self_from_claims(ctx.data_opt::<ClientClaims>())
            .map_err(KabiPayError::into_graphql)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let v = parse_uuid(&input.tax_config_version_id, "taxConfigVersionId")?;
        let m = tax_service::upsert_tax_computation(
            &db,
            tenant_id,
            employee_id,
            v,
            input.fiscal_year,
            input.tax_regime_chosen,
            tax_service::opt_decimal(&input.gross_income).map_err(KabiPayError::into_graphql)?,
            tax_service::opt_decimal(&input.total_deductions).map_err(KabiPayError::into_graphql)?,
            tax_service::opt_decimal(&input.taxable_income).map_err(KabiPayError::into_graphql)?,
            tax_service::opt_decimal(&input.final_tax).map_err(KabiPayError::into_graphql)?,
            tax_service::opt_decimal(&input.tds_per_month).map_err(KabiPayError::into_graphql)?,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxComputationDto::from(m))
    }

    /// Upsert **`tax_configuration_version`** — old/new regime rows per FY (HR tax admin).
    async fn upsert_tax_configuration_version(
        &self,
        ctx: &Context<'_>,
        input: UpsertTaxConfigurationVersionInput,
    ) -> Result<TaxConfigurationVersionDto> {
        require_tax_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let oid = input.id.as_ref().map(|id| parse_uuid(id, "id")).transpose()?;
        let m = tax_service::upsert_tax_configuration_version(
            &db,
            tenant_id,
            oid,
            input.fiscal_year,
            input.regime,
            input.country_code,
            input.is_active,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxConfigurationVersionDto::from(m))
    }

    /// Upsert **`tax_slab`** for a configuration version (HR tax admin).
    async fn upsert_tax_slab(
        &self,
        ctx: &Context<'_>,
        input: UpsertTaxSlabInput,
    ) -> Result<TaxSlabDto> {
        require_tax_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let sid = input.id.as_ref().map(|id| parse_uuid(id, "id")).transpose()?;
        let cfg = parse_uuid(&input.tax_config_version_id, "taxConfigVersionId")?;
        let from = Decimal::from_str(input.income_from.trim())
            .map_err(|_| KabiPayError::Validation("invalid incomeFrom decimal".into()))?;
        let to = tax_service::opt_decimal(&input.income_to).map_err(KabiPayError::into_graphql)?;
        let tr = tax_service::opt_decimal(&input.tax_rate).map_err(KabiPayError::into_graphql)?;
        let sr = tax_service::opt_decimal(&input.surcharge_rate).map_err(KabiPayError::into_graphql)?;
        let cr = tax_service::opt_decimal(&input.cess_rate).map_err(KabiPayError::into_graphql)?;
        let m = tax_service::upsert_tax_slab(
            &db,
            tenant_id,
            sid,
            cfg,
            from,
            to,
            tr,
            sr,
            cr,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxSlabDto::from(m))
    }

    /// Upsert a tenant tax deduction section definition (**`tax_proof_line.section_code`** catalogue).
    /// Same permission as approving proofs — HR tax admin.
    async fn upsert_tax_section_definition(
        &self,
        ctx: &Context<'_>,
        input: UpsertTaxSectionDefinitionInput,
    ) -> Result<TaxSectionDefinitionDto> {
        require_tax_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let max_d = tax_service::opt_decimal(&input.max_deduction_amount)?;
        let cc = input.country_code.unwrap_or_else(|| "IN".into());
        let disp = input.display_order.unwrap_or(0);
        let active = input.is_active.unwrap_or(true);
        let m = tax_service::upsert_tax_section_definition(
            &db,
            tenant_id,
            input.section_code,
            input.section_label,
            input.regime_scope,
            cc,
            disp,
            active,
            max_d,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxSectionDefinitionDto::from(m))
    }

    /// Submit or update a deduction **proof** line (declared vs actual). Resets status to **PENDING**
    /// until an approver accepts it. Only **APPROVED** lines sum into `tax_computation.totalDeductions`.
    async fn submit_tax_proof_line(
        &self,
        ctx: &Context<'_>,
        input: SubmitTaxProofLineInput,
    ) -> Result<TaxProofLineDto> {
        let employee_id = tax_submit_self_from_claims(ctx.data_opt::<ClientClaims>())
            .map_err(KabiPayError::into_graphql)?;
        let claims = ctx
            .data::<ClientClaims>()
            .map_err(|_| KabiPayError::Unauthorised.into_graphql())?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let tid = parse_uuid(&input.tax_config_version_id, "taxConfigVersionId")?;
        let declared = Decimal::from_str(input.declared_amount.trim())
            .map_err(|_| KabiPayError::Validation("invalid declaredAmount".into()))?;
        let actual = Decimal::from_str(input.actual_amount.trim())
            .map_err(|_| KabiPayError::Validation("invalid actualAmount".into()))?;
        let fid = parse_uuid(&input.file_storage_id, "fileStorageId")?;
        let m = tax_service::submit_tax_proof_line(
            &db,
            tenant_id,
            employee_id,
            claims.sub,
            tid,
            input.fiscal_year,
            input.section_code,
            declared,
            actual,
            fid,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxProofLineDto::from(m))
    }

    async fn approve_tax_proof_line(
        &self,
        ctx: &Context<'_>,
        tax_proof_line_id: ID,
    ) -> Result<TaxProofLineDto> {
        let (scope, approver_employee_id, approver_user_id) =
            tax_approval_actor_from_claims(ctx.data_opt::<ClientClaims>())
                .map_err(KabiPayError::into_graphql)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&tax_proof_line_id, "taxProofLineId")?;
        let m = tax_service::approve_tax_proof_line(
            &db,
            tenant_id,
            id,
            scope,
            approver_employee_id,
            approver_user_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxProofLineDto::from(m))
    }

    async fn reject_tax_proof_line(
        &self,
        ctx: &Context<'_>,
        tax_proof_line_id: ID,
        reason: Option<String>,
    ) -> Result<TaxProofLineDto> {
        let (scope, approver_employee_id, _approver_user_id) =
            tax_approval_actor_from_claims(ctx.data_opt::<ClientClaims>())
                .map_err(KabiPayError::into_graphql)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&tax_proof_line_id, "taxProofLineId")?;
        let m = tax_service::reject_tax_proof_line(
            &db,
            tenant_id,
            id,
            scope,
            approver_employee_id,
            reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TaxProofLineDto::from(m))
    }
}

fn exact_scope_from_claims(
    claims: Option<&ClientClaims>,
    permission: &'static str,
    accepted: &[ScopeType],
) -> KabiPayResult<ScopeType> {
    let scope = data_scope_from_claims(claims, permission)?;
    if !accepted.contains(&scope) {
        let scopes = accepted
            .iter()
            .map(|scope| scope.to_wire())
            .collect::<Vec<_>>()
            .join(" or ");
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires {scopes} scope"
        )));
    }
    Ok(scope)
}

fn tax_submit_self_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<Uuid> {
    exact_scope_from_claims(claims, PERM_TAX_SUBMIT, &[ScopeType::Self_])?;
    claims
        .and_then(|claims| claims.employee_id)
        .ok_or_else(|| KabiPayError::Forbidden("JWT-bound employee required".into()))
}

fn tax_manage_all_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    exact_scope_from_claims(claims, PERM_TAX_MANAGE, &[ScopeType::All])
}

fn tax_approve_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    exact_scope_from_claims(
        claims,
        PERM_TAX_APPROVE,
        &[ScopeType::Team, ScopeType::All],
    )
}

fn tax_approval_actor_from_claims(
    claims: Option<&ClientClaims>,
) -> KabiPayResult<(ScopeType, Uuid, Uuid)> {
    let scope = tax_approve_scope_from_claims(claims)?;
    let claims = claims.ok_or(KabiPayError::Unauthorised)?;
    let employee_id = claims
        .employee_id
        .ok_or_else(|| KabiPayError::Forbidden("JWT-bound employee required".into()))?;
    Ok((scope, employee_id, claims.sub))
}

fn require_tax_manage_all(ctx: &Context<'_>) -> Result<()> {
    tax_manage_all_from_claims(ctx.data_opt::<ClientClaims>())
        .map(|_| ())
        .map_err(KabiPayError::into_graphql)
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::{
        client_data_scope::EmployeeScopeFilter,
        context::{
            ClientClaims, CLIENT_JWT_ISSUER, PERM_TAX_APPROVE, PERM_TAX_MANAGE,
            PERM_TAX_SUBMIT,
        },
        subgraph::TenantId,
    };
    use std::collections::HashMap;

    use crate::resolvers::query::QueryRoot;

    const CONFIG_ID: &str = "00000000-0000-0000-0000-000000000001";
    const FILE_ID: &str = "00000000-0000-0000-0000-000000000002";
    const LINE_ID: &str = "00000000-0000-0000-0000-000000000003";

    fn claims(permission: &str, scope: Option<&str>, employee_id: Option<Uuid>) -> ClientClaims {
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

    async fn execute_mutation(claims: ClientClaims, mutation: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, MutationRoot, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(mutation))
            .await
    }

    fn assert_denied_before_db(response: &async_graphql::Response, expected: &str) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(message.contains(expected), "unexpected denial: {message}");
        assert!(!message.contains("TenantDbCache"));
    }

    fn assert_allowed_through_gate(response: &async_graphql::Response) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let code = response.errors[0]
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("code"))
            .cloned();
        assert_eq!(
            code,
            Some(async_graphql::Value::from("INTERNAL_ERROR")),
            "exact permission/scope should reach the database boundary: {response:?}"
        );
    }

    fn self_mutations() -> [String; 2] {
        [
            format!(
                "mutation {{ upsertTaxComputation(input: {{ taxConfigVersionId: \"{CONFIG_ID}\", fiscalYear: 2026 }}) {{ id }} }}"
            ),
            format!(
                "mutation {{ submitTaxProofLine(input: {{ taxConfigVersionId: \"{CONFIG_ID}\", fiscalYear: 2026, sectionCode: \"80C\", declaredAmount: \"1\", actualAmount: \"1\", fileStorageId: \"{FILE_ID}\" }}) {{ id }} }}"
            ),
        ]
    }

    fn manage_mutations() -> [String; 3] {
        [
            "mutation { upsertTaxConfigurationVersion(input: { fiscalYear: 2026, countryCode: \"IN\", isActive: true }) { id } }".into(),
            format!(
                "mutation {{ upsertTaxSlab(input: {{ taxConfigVersionId: \"{CONFIG_ID}\", incomeFrom: \"0\" }}) {{ id }} }}"
            ),
            "mutation { upsertTaxSectionDefinition(input: { sectionCode: \"80C\", sectionLabel: \"Section 80C\" }) { id } }".into(),
        ]
    }

    fn approval_mutations() -> [String; 2] {
        [
            format!(
                "mutation {{ approveTaxProofLine(taxProofLineId: \"{LINE_ID}\") {{ id }} }}"
            ),
            format!(
                "mutation {{ rejectTaxProofLine(taxProofLineId: \"{LINE_ID}\", reason: \"invalid\") {{ id }} }}"
            ),
        ]
    }

    #[test]
    fn exact_tax_permission_scopes_are_required() {
        let employee_id = Some(Uuid::new_v4());
        assert!(tax_submit_self_from_claims(Some(&claims(
            PERM_TAX_SUBMIT,
            Some("SELF"),
            employee_id,
        )))
        .is_ok());
        assert!(tax_manage_all_from_claims(Some(&claims(
            PERM_TAX_MANAGE,
            Some("ALL"),
            employee_id,
        )))
        .is_ok());
        for scope in ["TEAM", "ALL"] {
            assert!(tax_approve_scope_from_claims(Some(&claims(
                PERM_TAX_APPROVE,
                Some(scope),
                employee_id,
            )))
            .is_ok());
        }

        for denied in [
            claims(PERM_TAX_SUBMIT, None, employee_id),
            claims(PERM_TAX_SUBMIT, Some("TEAM"), employee_id),
            claims(PERM_TAX_SUBMIT, Some("ALL"), employee_id),
            claims(PERM_TAX_APPROVE, Some("SELF"), employee_id),
            claims(PERM_TAX_APPROVE, Some("DEPARTMENT"), employee_id),
            claims(PERM_TAX_MANAGE, Some("TEAM"), employee_id),
        ] {
            let permission = denied.permissions[0].as_str();
            let result = match permission {
                PERM_TAX_SUBMIT => tax_submit_self_from_claims(Some(&denied)).map(|_| ()),
                PERM_TAX_APPROVE => tax_approve_scope_from_claims(Some(&denied)).map(|_| ()),
                PERM_TAX_MANAGE => tax_manage_all_from_claims(Some(&denied)).map(|_| ()),
                _ => unreachable!(),
            };
            assert!(result.is_err(), "unexpectedly accepted {permission}");
        }
    }

    #[tokio::test]
    async fn every_tax_mutation_denies_wrong_permission_or_scope_before_database_access() {
        let employee_id = Some(Uuid::new_v4());
        for mutation in self_mutations() {
            let response = execute_mutation(
                claims(PERM_TAX_APPROVE, Some("ALL"), employee_id),
                &mutation,
            )
            .await;
            assert_denied_before_db(&response, "tax:submit permission required");
        }
        for mutation in manage_mutations() {
            let response = execute_mutation(
                claims(PERM_TAX_APPROVE, Some("ALL"), employee_id),
                &mutation,
            )
            .await;
            assert_denied_before_db(&response, "tax:manage permission required");
        }
        for mutation in approval_mutations() {
            let response = execute_mutation(
                claims(PERM_TAX_MANAGE, Some("ALL"), employee_id),
                &mutation,
            )
            .await;
            assert_denied_before_db(&response, "tax:approve permission required");
        }
    }

    #[tokio::test]
    async fn every_exact_allowed_tax_gate_reaches_database_boundary() {
        let employee_id = Some(Uuid::new_v4());
        for mutation in self_mutations() {
            assert_allowed_through_gate(
                &execute_mutation(
                    claims(PERM_TAX_SUBMIT, Some("SELF"), employee_id),
                    &mutation,
                )
                .await,
            );
        }
        for mutation in manage_mutations() {
            assert_allowed_through_gate(
                &execute_mutation(
                    claims(PERM_TAX_MANAGE, Some("ALL"), employee_id),
                    &mutation,
                )
                .await,
            );
        }
        for mutation in approval_mutations() {
            assert_allowed_through_gate(
                &execute_mutation(
                    claims(PERM_TAX_APPROVE, Some("TEAM"), employee_id),
                    &mutation,
                )
                .await,
            );
        }
    }

    #[test]
    fn approval_target_scope_enforces_team_membership_and_self_exclusion() {
        let approver_employee_id = Uuid::new_v4();
        let report_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let team = EmployeeScopeFilter::EmployeeIds(vec![approver_employee_id, report_id]);

        assert!(tax_service::require_tax_approval_target(
            &team,
            approver_employee_id,
            report_id,
        )
        .is_ok());
        assert!(tax_service::require_tax_approval_target(
            &EmployeeScopeFilter::Unrestricted,
            approver_employee_id,
            outside_id,
        )
        .is_ok());
        assert!(tax_service::require_tax_approval_target(
            &team,
            approver_employee_id,
            outside_id,
        )
        .is_err());
        assert!(tax_service::require_tax_approval_target(
            &EmployeeScopeFilter::Unrestricted,
            approver_employee_id,
            approver_employee_id,
        )
        .is_err());
    }
}
