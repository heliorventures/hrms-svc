use super::{
    authorization::{financial_error, transaction},
    types::*,
};
use async_graphql::{Context, Object, Result};
use kabipay_common::KabiPayError;
use kabipay_loans::{self as loans, LoanPermission};
use uuid::Uuid;
pub(super) fn uuid(value: &str) -> Result<Uuid> {
    let id: Uuid = value
        .parse()
        .map_err(|_| KabiPayError::Validation("invalid loan identifier".into()).into_graphql())?;
    if id.is_nil() {
        return Err(KabiPayError::Validation("invalid loan identifier".into()).into_graphql());
    }
    Ok(id)
}
pub(super) fn optional_uuid(value: Option<String>) -> Result<Option<Uuid>> {
    value.map(|value| uuid(&value)).transpose()
}
pub struct LoanQuery;
#[Object]
impl LoanQuery {
    async fn loan_policy_versions(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanPolicyConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Policy).await?;
        let page = loans::loan_policy_versions(&tx, &actor, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn preview_loan_reversal(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        posting_id: String,
    ) -> Result<LoanCorrectionPreviewDto> {
        let (tx, actor) = transaction(ctx, LoanPermission::Correct).await?;
        let review = loans::preview_loan_reversal(&tx, &actor, uuid(&loan_id)?, uuid(&posting_id)?)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(review.into())
    }
    async fn loan_payments(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanPaymentConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::loan_payments(&tx, &actor, uuid(&loan_id)?, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn loan_schedules(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanScheduleConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page =
            loans::loan_schedules(&tx, &actor, uuid(&loan_id)?, optional_uuid(after)?, first)
                .await
                .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn my_loans(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanAccountConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_accounts(&tx, &actor, true, None, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn loan_accounts(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<String>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanAccountConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_accounts(
            &tx,
            &actor,
            false,
            optional_uuid(employee_id)?,
            optional_uuid(after)?,
            first,
        )
        .await
        .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn my_loan_requests(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanRequestConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_requests(&tx, &actor, true, None, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn loan_request_queue(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<String>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanRequestConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_requests(
            &tx,
            &actor,
            false,
            optional_uuid(employee_id)?,
            optional_uuid(after)?,
            first,
        )
        .await
        .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn loan_account(&self, ctx: &Context<'_>, id: String) -> Result<LoanAccountDto> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let value = loans::loan_account(&tx, &actor, uuid(&id)?)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(value.into())
    }
    async fn loan_policies(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanPolicyConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_policies(&tx, &actor, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
    async fn loan_ledger(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanPostingConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::loan_ledger(&tx, &actor, uuid(&loan_id)?, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(page.into())
    }
}
