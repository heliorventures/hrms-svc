use super::{draft, source, types::*};
use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::ScopeType,
    finance_contract::{ExitSubject, FinanceAction},
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    tenant_business_clock::TenantBusinessClock,
    KabiPayError,
};
use uuid::Uuid;

fn id(value: &ID) -> Result<Uuid> {
    value
        .parse()
        .ok()
        .filter(|v: &Uuid| !v.is_nil())
        .ok_or_else(|| {
            KabiPayError::Validation("Invalid separation reference".into()).into_graphql()
        })
}
fn manage(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if kabipay_common::client_data_scope::data_scope_from_claims(Some(claims), "onboarding:manage")
        .map_err(KabiPayError::into_graphql)?
        != ScopeType::All
    {
        return Err(
            KabiPayError::Forbidden("FNF management requires ALL HR scope".into()).into_graphql(),
        );
    }
    Ok(())
}
async fn subject(ctx: &Context<'_>, sep: Uuid) -> Result<ExitSubject> {
    let tenant = require_tenant_id(ctx)?;
    let value = super::http::hr_read(
        tenant,
        FinanceAction::ReadSeparation,
        serde_json::json!({"separationId":sep}),
    )
    .await
    .map_err(KabiPayError::into_graphql)?;
    let subject: ExitSubject = serde_json::from_value(value).map_err(|_| {
        KabiPayError::Conflict("HR separation could not be verified".into()).into_graphql()
    })?;
    if subject.separation_id != sep {
        return Err(
            KabiPayError::Forbidden("Separation binding does not match".into()).into_graphql(),
        );
    }
    Ok(subject)
}
async fn today(ctx: &Context<'_>, tenant: Uuid) -> Result<chrono::NaiveDate> {
    let ops = ctx.data::<sea_orm::DatabaseConnection>().map_err(|_| {
        KabiPayError::Internal("Finance clock configuration unavailable".into()).into_graphql()
    })?;
    Ok(TenantBusinessClock::load(ops, tenant)
        .await
        .map_err(KabiPayError::into_graphql)?
        .now_date())
}

pub struct FnfQuery;
#[Object]
impl FnfQuery {
    async fn preview_fnf_loan_exposure(
        &self,
        ctx: &Context<'_>,
        separation_id: Uuid,
        settlement_date: chrono::NaiveDate,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        use sea_orm::TransactionTrait;
        manage(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let subject = subject(ctx, separation_id).await?;
        if subject.status != "APPROVED" {
            return Err(KabiPayError::Conflict("FNF review requires an approved separation".into()).into_graphql());
        }
        let actor = super::review::actor(require_client_claims(ctx)?, tenant).map_err(KabiPayError::into_graphql)?;
        let db = tenant_db(ctx, tenant).await?;
        let tx = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let exposure = kabipay_loans::preview_exit_exposure(&tx, &actor, subject.employee_id, settlement_date).await
            .map_err(super::coordination::financial_error).map_err(KabiPayError::into_graphql)?;
        let state = kabipay_loans::payroll_loan_state(&tx, tenant).await
            .map_err(super::coordination::financial_error).map_err(KabiPayError::into_graphql)?;
        let payroll_context = super::payroll_context::read(&tx, tenant, subject.employee_id).await.map_err(KabiPayError::into_graphql)?;
        tx.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(async_graphql::Json(serde_json::json!({"exposure":exposure,"companyCurrency":state.currency,"payrollContext":payroll_context})))
    }
    async fn finance_follow_up_owners(
        &self,
        ctx: &Context<'_>,
        after: Option<Uuid>,
        #[graphql(default = 25)] first: u64,
    ) -> Result<FinanceOwnerConnection> {
        use kabipay_db_entities::tenant::d0005_auth_rbac::user;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
        manage(ctx)?;
        if first == 0 || first > 100 {
            return Err(KabiPayError::Validation("Choose a page size from 1 to 100".into()).into_graphql());
        }
        let tenant = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant).await?;
        let mut query = user::Entity::find().filter(user::Column::TenantId.eq(tenant))
            .filter(user::Column::IsActive.eq(true)).filter(user::Column::IsDeleted.eq(false));
        if let Some(after) = after { query = query.filter(user::Column::Id.gt(after)); }
        let mut rows = query.order_by_asc(user::Column::Id).limit(first + 1).all(&db).await
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let has_next_page = rows.len() > first as usize;
        rows.truncate(first as usize);
        let end_cursor = rows.last().map(|row| row.id.to_string());
        Ok(FinanceOwnerConnection { nodes: rows.into_iter().map(|row| FinanceOwner {id:row.id,name:row.username}).collect(),
            page_info: FinanceOwnerPageInfo {end_cursor,has_next_page} })
    }
    async fn fnf_settlement(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
    ) -> Result<Option<FnfSettlementDto>> {
        let claims = require_client_claims(ctx)?;
        kabipay_common::finance_contract::exit_visibility_employee(claims)
            .map_err(KabiPayError::into_graphql)?;
        let tenant = require_tenant_id(ctx)?;
        let value = kabipay_common::finance_transport::send(
            kabipay_common::finance_contract::ServiceCaller::Payroll,
            kabipay_common::finance_contract::ServiceCaller::Employee,
            tenant,
            Some(claims),
            FinanceAction::ReadSeparation,
            serde_json::json!({"separationId":id(&separation_id)?,"publicRead":true}),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let visible: Option<ExitSubject> = serde_json::from_value(value).map_err(|_| {
            KabiPayError::Conflict("HR separation could not be verified".into()).into_graphql()
        })?;
        let Some(subject) = visible else {
            return Ok(None);
        };
        if subject.separation_id != id(&separation_id)? {
            return Err(
                KabiPayError::Forbidden("Separation binding does not match".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant).await?;
        match source::find(&db, tenant, subject.separation_id, false)
            .await
            .map_err(KabiPayError::into_graphql)?
        {
            None => Ok(None),
            Some(row) => {
                let review = draft::load(&db, tenant, row.id, false)
                    .await
                    .map_err(KabiPayError::into_graphql)?;
                Ok(Some(FnfSettlementDto::new(row, review.as_ref())))
            }
        }
    }
    async fn fnf_financial_review(
        &self,
        ctx: &Context<'_>,
        separation_id: Uuid,
    ) -> Result<FnfFinancialPreparation> {
        manage(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let subject = subject(ctx, separation_id).await?;
        let db = tenant_db(ctx, tenant).await?;
        let row = source::find(&db, tenant, subject.separation_id, false)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let review = match &row {
            Some(row) => draft::load(&db, tenant, row.id, false)
                .await
                .map_err(KabiPayError::into_graphql)?,
            None => None,
        };
        let legacy = match &row {
            Some(row) if review.is_none() => {
                Some(draft::legacy_fingerprint(row).map_err(KabiPayError::into_graphql)?)
            }
            _ => None,
        };
        Ok(FnfFinancialPreparation {
            revision: review.as_ref().map(|v| v.revision),
            fingerprint: review.as_ref().map(|v| v.fingerprint.clone()),
            input: review
                .as_ref()
                .map(|v| serde_json::to_value(&v.snapshot.input))
                .transpose()
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)?
                .map(async_graphql::Json),
            settlement: row.map(|row| FnfSettlementDto::new(row, review.as_ref())),
            legacy_figures_fingerprint: legacy,
        })
    }
}

pub struct FnfMutation;
#[Object]
impl FnfMutation {
    async fn upsert_fnf_settlement(
        &self,
        ctx: &Context<'_>,
        input: UpsertFnfSettlementInput,
    ) -> Result<FnfSettlementDto> {
        manage(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let subject = subject(ctx, id(&input.separation_id)?).await?;
        let db = tenant_db(ctx, tenant).await?;
        let row = super::legacy::save(
            &db,
            tenant,
            require_client_claims(ctx)?,
            &subject,
            today(ctx, tenant).await?,
            &input.leave_encashment,
            &input.gratuity_amount,
            &input.bonus_payable,
            &input.recovery_amount,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(FnfSettlementDto::new(row, None))
    }
    async fn save_fnf_financial_review(
        &self,
        ctx: &Context<'_>,
        input: SaveFnfFinancialReviewInput,
    ) -> Result<FnfFinancialPreparation> {
        manage(ctx)?;
        let tenant = require_tenant_id(ctx)?;
        let subject = subject(ctx, input.separation_id).await?;
        let financial = serde_json::from_value(input.input.0).map_err(|_| {
            KabiPayError::Validation("FNF source review is incomplete".into()).into_graphql()
        })?;
        let db = tenant_db(ctx, tenant).await?;
        draft::save(
            &db,
            tenant,
            require_client_claims(ctx)?,
            &subject,
            &financial,
            input.expected_revision,
            input.expected_fingerprint.as_deref(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        FnfQuery
            .fnf_financial_review(ctx, input.separation_id)
            .await
    }
    async fn finalize_fnf_settlement(
        &self,
        ctx: &Context<'_>,
        separation_id: ID,
        expected_revision: Option<i64>,
        expected_fingerprint: Option<String>,
    ) -> Result<FnfSettlementDto> {
        manage(ctx)?;
        let revision = expected_revision.ok_or_else(|| {
            KabiPayError::Validation(
                "Review the complete FNF calculation before finalization".into(),
            )
            .into_graphql()
        })?;
        let fingerprint = expected_fingerprint.ok_or_else(|| {
            KabiPayError::Validation("Reviewed FNF fingerprint is required".into()).into_graphql()
        })?;
        let tenant = require_tenant_id(ctx)?;
        let subject = subject(ctx, id(&separation_id)?).await?;
        let db = tenant_db(ctx, tenant).await?;
        let issued = super::finalize::finalize(
            &db,
            tenant,
            require_client_claims(ctx)?,
            &subject,
            revision,
            &fingerprint,
            today(ctx, tenant).await?,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let row = source::find(&db, tenant, subject.separation_id, false)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| {
                KabiPayError::Internal("Issued FNF could not be read".into()).into_graphql()
            })?;
        Ok(FnfSettlementDto::new(row, Some(&issued)))
    }
}
