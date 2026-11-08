//! Disbursement status is read from HR while the shared financial lock is held.
use super::{
    authorization::{financial_error, transaction},
    types::LoanResultDto,
};
use async_graphql::{Context, Result};
use kabipay_common::{
    finance_contract::{FinanceAction, FundingContext, ServiceCaller},
    finance_transport,
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
use kabipay_loans::{CommandMeta, LoanCommand, LoanPermission};

pub(super) async fn record(
    ctx: &Context<'_>,
    command: LoanCommand,
    meta: CommandMeta,
) -> Result<LoanResultDto> {
    let (loan_id, payment) = match &command {
        LoanCommand::RecordDisbursement { loan_id, payment }
        | LoanCommand::RecordHistoricalPayment {
            loan_id,
            payment,
            kind: kabipay_loans::HistoricalPaymentKind::Disbursement,
            ..
        } => (loan_id, payment),
        _ => {
            return Err(
                KabiPayError::Validation("Disbursement command is required".into()).into_graphql(),
            )
        }
    };
    let tenant = require_tenant_id(ctx)?;
    let claims = require_client_claims(ctx)?;
    let (tx, actor) = transaction(ctx, LoanPermission::Disburse).await?;
    if let Some(result) = kabipay_loans::recorded_command(&tx, &actor, &meta, &command)
        .await
        .map_err(financial_error)?
    {
        tx.commit()
            .await
            .map_err(|error| financial_error(error.into()))?;
        return Ok(result.into());
    }
    let employee = kabipay_loans::funding_employee(&tx, &actor, *loan_id)
        .await
        .map_err(financial_error)?;
    let response = finance_transport::send(
        ServiceCaller::Payroll,
        ServiceCaller::Employee,
        tenant,
        Some(claims),
        FinanceAction::ReadFundingContext,
        serde_json::json!({"employeeId":employee,"valueDate":payment.value_date}),
    )
    .await
    .map_err(KabiPayError::into_graphql)?;
    let context: FundingContext = serde_json::from_value(response).map_err(|_| {
        KabiPayError::Conflict("HR funding status could not be verified".into()).into_graphql()
    })?;
    if context.employee_id != employee || context.value_date != payment.value_date {
        return Err(
            KabiPayError::Conflict("HR funding status binding changed".into()).into_graphql(),
        );
    }
    kabipay_common::funding_exception::validate(
        claims,
        context.pending_separation_id.is_some(),
        payment.resignation_exception_reason.as_deref(),
    )
    .map_err(KabiPayError::into_graphql)?;
    let result =
        kabipay_loans::execute_with_funding_context(&tx, &actor, &meta, &command, Some(&context))
            .await
            .map_err(financial_error)?;
    tx.commit()
        .await
        .map_err(|error| financial_error(error.into()))?;
    Ok(result.into())
}
