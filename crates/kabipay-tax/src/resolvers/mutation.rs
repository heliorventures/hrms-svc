//! Write operations for employee tax computations / declarations.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
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
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_tax_proof() {
            return Err(
                KabiPayError::Forbidden(
                    "manage tax configuration requires tax:approve or HR / tenant admin role".into(),
                )
                .into_graphql(),
            );
        }
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_tax_proof() {
            return Err(
                KabiPayError::Forbidden(
                    "manage tax slabs requires tax:approve or HR / tenant admin role".into(),
                )
                .into_graphql(),
            );
        }
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_tax_proof() {
            return Err(
                KabiPayError::Forbidden(
                    "manage tax sections requires tax:approve or HR / tenant admin role".into(),
                )
                .into_graphql(),
            );
        }
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
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_tax_proof() {
            return Err(
                KabiPayError::Forbidden(
                    "tax proof approve permission required (tax:approve or HR / tenant admin role)"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&tax_proof_line_id, "taxProofLineId")?;
        let m = tax_service::approve_tax_proof_line(&db, tenant_id, id, claims.sub)
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
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_tax_proof() {
            return Err(
                KabiPayError::Forbidden(
                    "tax proof approve permission required (tax:approve or HR / tenant admin role)"
                        .into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&tax_proof_line_id, "taxProofLineId")?;
        let m = tax_service::reject_tax_proof_line(&db, tenant_id, id, reason)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(TaxProofLineDto::from(m))
    }
}
