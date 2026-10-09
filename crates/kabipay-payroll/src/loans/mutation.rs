use super::{
    authorization::{financial_error, transaction},
    query::{optional_uuid, uuid},
    types::*,
};
use async_graphql::{Context, Object, Result};
use chrono::NaiveDate;
use kabipay_common::KabiPayError;
use kabipay_loans::{execute_command, CommandMeta, LoanCommand, RecordedPayment, RequestDecision};
use kabipay_loans_domain::{Currency, RecoveryMode};
use rust_decimal::Decimal;
fn date(value: &str) -> Result<NaiveDate> {
    let date: NaiveDate = value.parse().map_err(|_| {
        KabiPayError::Validation("invalid loan date; use YYYY-MM-DD".into()).into_graphql()
    })?;
    if date.to_string() != value {
        return Err(
            KabiPayError::Validation("invalid loan date; use YYYY-MM-DD".into()).into_graphql(),
        );
    }
    Ok(date)
}
fn optional_date(value: Option<String>) -> Result<Option<NaiveDate>> {
    value.map(|value| date(&value)).transpose()
}
fn amount(value: &str) -> Result<Decimal> {
    serde_json::from_value::<DecimalString>(serde_json::Value::String(value.into()))
        .map(|value| value.0)
        .map_err(|_| {
            KabiPayError::Validation("use a plain decimal string for loan amounts".into())
                .into_graphql()
        })
}
#[derive(serde::Deserialize)]
struct DecimalString(#[serde(with = "kabipay_loans_domain::decimal_string")] Decimal);
async fn run(
    ctx: &Context<'_>,
    input: LoanCommand,
    meta: LoanCommandMetaInput,
) -> Result<LoanResultDto> {
    let (tx, actor) = transaction(ctx, input.permission()).await?;
    let meta = CommandMeta {
        idempotency_key: meta.idempotency_key,
        expected_version: meta.expected_version,
    };
    let result = execute_command(&tx, &actor, &meta, &input)
        .await
        .map_err(financial_error)?;
    tx.commit()
        .await
        .map_err(|error| financial_error(error.into()))?;
    Ok(result.into())
}
fn payment(input: &RecordLoanPaymentInput) -> Result<RecordedPayment> {
    Ok(RecordedPayment {
        amount: amount(&input.amount)?,
        value_date: date(&input.value_date)?,
        method: input.method.clone(),
        external_reference: input.external_reference.clone(),
        evidence_reference: input.evidence_reference.clone(),
    })
}
pub struct LoanMutation;
#[Object]
impl LoanMutation {
    async fn retire_loan_policy(
        &self,
        ctx: &Context<'_>,
        input: RetireLoanPolicyInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::RetirePolicy {
                policy_id: uuid(&input.policy_id)?,
                reason: input.reason,
            },
            input.meta,
        )
        .await
    }
    async fn save_loan_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitLoanRequestInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::SaveRequest {
                request_id: optional_uuid(input.request_id)?,
                employee_id: optional_uuid(input.employee_id)?,
                policy_id: uuid(&input.policy_id)?,
                amount: amount(&input.amount)?,
                purpose: input.purpose,
                notes: input.notes,
                preferences: input.preferences.0,
            },
            input.meta,
        )
        .await
    }
    async fn publish_loan_policy(
        &self,
        ctx: &Context<'_>,
        input: PublishLoanPolicyInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::PublishPolicy {
                key: input.key,
                currency: Currency {
                    code: input.currency,
                    minor_units: input.minor_units,
                },
                effective_from: date(&input.effective_from)?,
                effective_to: optional_date(input.effective_to)?,
                rules: serde_json::from_value(input.rules.0)
                    .map_err(|_| financial_error(kabipay_loans::LoanModuleError::InvalidCommand))?,
            },
            input.meta,
        )
        .await
    }
    async fn submit_loan_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitLoanRequestInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::SubmitRequest {
                request_id: optional_uuid(input.request_id)?,
                employee_id: optional_uuid(input.employee_id)?,
                policy_id: uuid(&input.policy_id)?,
                amount: amount(&input.amount)?,
                purpose: input.purpose,
                notes: input.notes,
                preferences: input.preferences.0,
            },
            input.meta,
        )
        .await
    }
    async fn decide_loan_request(
        &self,
        ctx: &Context<'_>,
        input: DecideLoanRequestInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::DecideRequest {
                request_id: uuid(&input.request_id)?,
                step_id: uuid(&input.step_id)?,
                decision: match input.decision {
                    LoanRequestDecision::Approve => RequestDecision::Approve,
                    LoanRequestDecision::Return => RequestDecision::Return,
                    LoanRequestDecision::Reject => RequestDecision::Reject,
                },
                reason: input.reason,
                terms: input
                    .terms
                    .map(|v| serde_json::from_value(v.0))
                    .transpose()
                    .map_err(|_| financial_error(kabipay_loans::LoanModuleError::InvalidCommand))?,
                effective_from: optional_date(input.effective_from)?,
                first_due_date: optional_date(input.first_due_date)?,
                agreement_reference: input.agreement_reference,
            },
            input.meta,
        )
        .await
    }
    async fn withdraw_loan_request(
        &self,
        ctx: &Context<'_>,
        input: WithdrawLoanRequestInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::WithdrawRequest {
                request_id: uuid(&input.request_id)?,
                reason: input.reason,
            },
            input.meta,
        )
        .await
    }
    async fn record_loan_disbursement(
        &self,
        ctx: &Context<'_>,
        input: RecordLoanPaymentInput,
    ) -> Result<LoanResultDto> {
        let command = LoanCommand::RecordDisbursement {
            loan_id: uuid(&input.loan_id)?,
            payment: payment(&input)?,
        };
        run(ctx, command, input.meta).await
    }
    async fn record_loan_receipt(
        &self,
        ctx: &Context<'_>,
        input: RecordLoanPaymentInput,
    ) -> Result<LoanResultDto> {
        let command = LoanCommand::RecordReceipt {
            loan_id: uuid(&input.loan_id)?,
            payment: payment(&input)?,
        };
        run(ctx, command, input.meta).await
    }
    async fn set_loan_deduction(
        &self,
        ctx: &Context<'_>,
        input: SetLoanDeductionInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::SetDeduction {
                loan_id: uuid(&input.loan_id)?,
                effective_from: date(&input.effective_from)?,
                first_due_date: date(&input.first_due_date)?,
                amount: amount(&input.amount)?,
                recovery: match input.recovery {
                    LoanRecoveryMode::Payroll => RecoveryMode::Payroll,
                    LoanRecoveryMode::External => RecoveryMode::External,
                    LoanRecoveryMode::Mixed => RecoveryMode::Mixed,
                },
                reason: input.reason,
            },
            input.meta,
        )
        .await
    }
    async fn set_loan_period_override(
        &self,
        ctx: &Context<'_>,
        input: SetLoanPeriodOverrideInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::SetPeriodOverride {
                loan_id: uuid(&input.loan_id)?,
                period_start: date(&input.period_start)?,
                amount: input.amount.map(|value| amount(&value)).transpose()?,
                pause_interest: input.pause_interest,
                reason: input.reason,
            },
            input.meta,
        )
        .await
    }
    async fn reverse_loan_posting(
        &self,
        ctx: &Context<'_>,
        input: ReverseLoanPostingInput,
    ) -> Result<LoanResultDto> {
        run(
            ctx,
            LoanCommand::ReversePosting {
                loan_id: uuid(&input.loan_id)?,
                posting_id: uuid(&input.posting_id)?,
                reason: input.reason,
                reconciliation_reference: input.reconciliation_reference,
                review_fingerprint: input.review_fingerprint,
            },
            input.meta,
        )
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn financial_input_rejects_scientific_notation_spaces_and_float_coercion() {
        for value in ["1e3", " 100", "100 ", "NaN", "1,000"] {
            assert!(amount(value).is_err(), "{value}")
        }
        assert_eq!(amount("100.25").unwrap(), Decimal::new(10025, 2));
    }
}
