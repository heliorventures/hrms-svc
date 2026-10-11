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
    async fn preview_loan_deduction(
        &self,
        ctx: &Context<'_>,
        input: SetLoanDeductionInput,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        let command = super::mutation::deduction_command(&input)?;
        preview_arrangement(ctx, input.meta.expected_version, command).await
    }
    async fn preview_loan_period_override(
        &self,
        ctx: &Context<'_>,
        input: SetLoanPeriodOverrideInput,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        let command = super::mutation::period_override_command(&input)?;
        preview_arrangement(ctx, input.meta.expected_version, command).await
    }
    async fn preview_historical_loan_payment(
        &self,
        ctx: &Context<'_>,
        input: RecordLoanPaymentInput,
        kind: LoanHistoricalPaymentKind,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        let kind: loans::HistoricalPaymentKind = kind.into();
        let (tx, actor) = transaction(ctx, kind.permission()).await?;
        let review = loans::preview_historical_payment(
            &tx,
            &actor,
            uuid(&input.loan_id)?,
            kind,
            &super::mutation::payment(&input)?,
        )
        .await
        .map_err(financial_error)?;
        let value = serde_json::to_value(review).map_err(|e| financial_error(e.into()))?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(async_graphql::Json(value))
    }
    async fn loan_financial_reconciliation(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
    ) -> Result<Option<async_graphql::Json<serde_json::Value>>> {
        let (tx, actor) = transaction(ctx, loans::LoanPermission::Manage).await?;
        let report = loans::latest_loan_audit(&tx, &actor, uuid(&loan_id)?)
            .await
            .map_err(financial_error)?;
        let result = report
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| financial_error(e.into()))?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(result.map(async_graphql::Json))
    }
    async fn loan_policy(&self, ctx: &Context<'_>, id: String) -> Result<LoanPolicyDto> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let value = loans::loan_policy(&tx, &actor, uuid(&id)?)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(value.into())
    }
    async fn preview_loan_approval(
        &self,
        ctx: &Context<'_>,
        request_id: String,
        terms: async_graphql::Json<serde_json::Value>,
        effective_from: chrono::NaiveDate,
        first_due_date: chrono::NaiveDate,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        let (tx, actor) = transaction(ctx, LoanPermission::Approve).await?;
        let terms = serde_json::from_value(terms.0)
            .map_err(|_| financial_error(loans::LoanModuleError::InvalidCommand))?;
        let result = loans::preview_approval_schedule(
            &tx,
            &actor,
            uuid(&request_id)?,
            &terms,
            effective_from,
            first_due_date,
        )
        .await
        .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(async_graphql::Json(result))
    }
    async fn loan_schedule_preview(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        first_due_date: Option<chrono::NaiveDate>,
    ) -> Result<async_graphql::Json<serde_json::Value>> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        // Optional only for older clients; current screens use contractual dates.
        let result = if let Some(first) = first_due_date {
            loans::loan_schedule_preview(&tx, &actor, uuid(&loan_id)?, first).await
        } else {
            loans::current_loan_schedule_preview(&tx, &actor, uuid(&loan_id)?).await
        }.map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(async_graphql::Json(result))
    }
    async fn loan_statement(
        &self,
        ctx: &Context<'_>,
        loan_id: String,
        expected_version: i64,
        after: Option<String>,
        #[graphql(default = 100)] first: u32,
    ) -> Result<LoanPostingConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Export).await?;
        let loan = uuid(&loan_id)?;
        let account = loans::loan_account(&tx, &actor, loan)
            .await
            .map_err(financial_error)?;
        if account.version != expected_version {
            return Err(financial_error(loans::LoanModuleError::VersionConflict));
        }
        let result = loans::loan_ledger(&tx, &actor, loan, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(result.into())
    }
    async fn loan_approval_workflows(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanApprovalWorkflowConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Policy).await?;
        let result = loans::loan_approval_workflows(&tx, &actor, optional_uuid(after)?, first)
            .await
            .map_err(financial_error)?;
        tx.commit().await.map_err(|e| financial_error(e.into()))?;
        Ok(result.into())
    }
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
        search: Option<String>,
        state: Option<String>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanAccountConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_filtered_accounts(
            &tx,
            &actor,
            false,
            optional_uuid(employee_id)?,
            optional_uuid(after)?,
            first,
            &loans::LoanReadFilter { search, state },
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
        search: Option<String>,
        state: Option<String>,
        after: Option<String>,
        #[graphql(default = 25)] first: u32,
    ) -> Result<LoanRequestConnection> {
        let (tx, actor) = transaction(ctx, LoanPermission::Read).await?;
        let page = loans::list_filtered_requests(
            &tx,
            &actor,
            false,
            optional_uuid(employee_id)?,
            optional_uuid(after)?,
            first,
            &loans::LoanReadFilter { search, state },
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
async fn preview_arrangement(
    ctx: &Context<'_>,
    expected: i64,
    command: loans::LoanCommand,
) -> Result<async_graphql::Json<serde_json::Value>> {
    let (tx, actor) = transaction(ctx, LoanPermission::Manage).await?;
    let value = loans::preview_arrangement_change(&tx, &actor, expected, &command)
        .await
        .map_err(financial_error)?;
    tx.commit()
        .await
        .map_err(|error| financial_error(error.into()))?;
    Ok(async_graphql::Json(value))
}
