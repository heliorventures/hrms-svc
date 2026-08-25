//! Root query resolvers for kabipay-tax.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    TaxComputationDto, TaxConfigurationVersionDto, TaxProofLineDto, TaxSectionDefinitionDto,
    TaxSlabDto,
};
use crate::services::tax_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn tax_health(&self) -> &'static str {
        "ok"
    }

    /// Tax configuration versions configured for this tenant.
    async fn tax_configurations(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 20)] limit: u64,
    ) -> Result<Vec<TaxConfigurationVersionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_configurations(&db, tenant_id, active_only, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(TaxConfigurationVersionDto::from)
            .collect())
    }

    /// Deduction sections catalogue (**`tax_proof_line.section_code`**) — admin-maintained labels & caps.
    async fn tax_section_definitions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TaxSectionDefinitionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_tax_section_definitions(&db, tenant_id, active_only, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(TaxSectionDefinitionDto::from)
            .collect())
    }

    /// Tax slabs for this tenant (filter by fiscal_year server-side later).
    async fn tax_slabs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TaxSlabDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_slabs(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TaxSlabDto::from).collect())
    }

    /// Stored per-employee tax computation / declaration rows for a fiscal period.
    /// Omit `employeeId` to use the signed-in user’s employee record.
    async fn tax_computations(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 20)] limit: u64,
    ) -> Result<Vec<TaxComputationDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(id) = &employee_id {
            parse_uuid(id, "employeeId")?
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        let rows = tax_service::list_computations(&db, tenant_id, emp, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TaxComputationDto::from).collect())
    }

    /// Deduction proof lines (declared vs actual) for an employee. Omit `employeeId` for self;
    /// viewing another employee requires `tax:approve`.
    async fn tax_proof_lines(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        tax_config_version_id: Option<ID>,
        fiscal_year: Option<i32>,
    ) -> Result<Vec<TaxProofLineDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let self_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let target = if let Some(id) = &employee_id {
            parse_uuid(id, "employeeId")?
        } else {
            self_id
        };
        if target != self_id {
            let claims = require_client_claims(ctx)?;
            if !claims.can_approve_tax_proof() {
                return Err(
                    KabiPayError::Forbidden(
                        "cannot read another employee's tax proofs without tax:approve (or HR / admin role)"
                            .into(),
                    )
                    .into_graphql(),
                );
            }
        }
        let emp = target;
        let cfg = tax_config_version_id
            .as_ref()
            .map(|id| parse_uuid(id, "taxConfigVersionId"))
            .transpose()?;
        let rows = tax_service::list_tax_proof_lines(&db, tenant_id, emp, cfg, fiscal_year)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TaxProofLineDto::from).collect())
    }
}
