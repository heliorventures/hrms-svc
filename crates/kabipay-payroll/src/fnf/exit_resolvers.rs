//! Finance reads Employee's authoritative separation before entering a financial transaction.
use async_graphql::{Context, InputObject, Json, Object, Result, SimpleObject};
use chrono::NaiveDate;
use kabipay_common::{
    finance_contract::{ExitSubject, FinanceAction, ServiceCaller},
    finance_transport,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_loans::{LoanActorScope, LoanPermission, SaveExitReview};
use sea_orm::TransactionTrait;
use serde_json::Value;
use uuid::Uuid;

#[derive(SimpleObject)]
pub struct LoanExitReviewPreview {
    pub separation_id: Uuid,
    pub employee_id: Uuid,
    pub last_working_date: NaiveDate,
    pub separation_fingerprint: String,
    pub exposure: Json<Value>,
    pub payroll_context: Json<Value>,
}

#[derive(InputObject)]
pub struct LoanExitReviewInput {
    pub separation_id: Uuid,
    pub settlement_date: NaiveDate,
    pub separation_fingerprint: String,
    pub exposure_fingerprint: String,
    pub decision: Json<Value>,
}

#[derive(SimpleObject)]
pub struct LoanExitReviewResult {
    pub id: Uuid,
    pub separation_id: Uuid,
    pub exposure: Json<Value>,
    pub decision: Json<Value>,
}

async fn subject(
    ctx: &Context<'_>,
    separation: Uuid,
) -> Result<(Uuid, LoanActorScope, ExitSubject)> {
    let tenant = require_tenant_id(ctx)?;
    let claims = require_client_claims(ctx)?;
    if kabipay_common::client_data_scope::data_scope_from_claims(Some(claims), "loan:exit-review")
        .map_err(KabiPayError::into_graphql)?
        != kabipay_common::context::ScopeType::All
    {
        return Err(
            KabiPayError::Forbidden("Exit financial review requires ALL scope".into())
                .into_graphql(),
        );
    }
    let actor = LoanActorScope::from_verified_claims(claims, tenant, LoanPermission::ExitReview)
        .map_err(super::coordination::financial_error)
        .map_err(KabiPayError::into_graphql)?;
    if separation.is_nil() {
        return Err(
            KabiPayError::Validation("Separation reference is required".into()).into_graphql(),
        );
    }
    let value = finance_transport::send(
        ServiceCaller::Payroll,
        ServiceCaller::Employee,
        tenant,
        None,
        FinanceAction::ReadSeparation,
        serde_json::json!({"separationId":separation}),
    )
    .await
    .map_err(KabiPayError::into_graphql)?;
    let subject: ExitSubject = serde_json::from_value(value).map_err(|_| {
        KabiPayError::Conflict("HR separation could not be verified".into()).into_graphql()
    })?;
    if subject.separation_id != separation || subject.status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "Exit financial review requires a pending separation".into(),
        )
        .into_graphql());
    }
    Ok((tenant, actor, subject))
}

pub struct ExitReviewQuery;
#[Object]
impl ExitReviewQuery {
    async fn preview_loan_exit_review(
        &self,
        ctx: &Context<'_>,
        separation_id: Uuid,
        settlement_date: NaiveDate,
    ) -> Result<LoanExitReviewPreview> {
        let (tenant, actor, subject) = subject(ctx, separation_id).await?;
        let db = tenant_db(ctx, tenant).await?;
        let tx = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let exposure =
            kabipay_loans::preview_exit_exposure(&tx, &actor, subject.employee_id, settlement_date)
                .await
                .map_err(super::coordination::financial_error)
                .map_err(KabiPayError::into_graphql)?;
        let payroll_context = super::payroll_context::read(&tx, tenant, subject.employee_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        tx.commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(LoanExitReviewPreview {
            separation_id,
            employee_id: subject.employee_id,
            last_working_date: subject.last_working_date,
            separation_fingerprint: subject.fingerprint,
            payroll_context: Json(payroll_context),
            exposure: Json(
                serde_json::to_value(exposure)
                    .map_err(KabiPayError::from)
                    .map_err(KabiPayError::into_graphql)?,
            ),
        })
    }
}

pub struct ExitReviewMutation;
#[Object]
impl ExitReviewMutation {
    async fn save_loan_exit_review(
        &self,
        ctx: &Context<'_>,
        input: LoanExitReviewInput,
    ) -> Result<LoanExitReviewResult> {
        let (tenant, actor, subject) = subject(ctx, input.separation_id).await?;
        if subject.fingerprint != input.separation_fingerprint {
            return Err(KabiPayError::Conflict(
                "Separation changed; refresh and review it again".into(),
            )
            .into_graphql());
        }
        let decision = serde_json::from_value(input.decision.0).map_err(|_| {
            KabiPayError::Validation("Exit recovery decision is incomplete".into()).into_graphql()
        })?;
        let db = tenant_db(ctx, tenant).await?;
        let tx = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        super::decision_authority::verify(&tx, tenant, subject.employee_id, &decision)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let review = kabipay_loans::save_exit_review(
            &tx,
            &actor,
            &SaveExitReview {
                separation_id: subject.separation_id,
                employee_id: subject.employee_id,
                separation_fingerprint: subject.fingerprint,
                last_working_date: subject.last_working_date,
                settlement_date: input.settlement_date,
                exposure_fingerprint: input.exposure_fingerprint,
                decision,
            },
        )
        .await
        .map_err(super::coordination::financial_error)
        .map_err(KabiPayError::into_graphql)?;
        tx.commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(LoanExitReviewResult {
            id: review.id,
            separation_id: review.separation_id,
            exposure: Json(
                serde_json::to_value(review.exposure)
                    .map_err(KabiPayError::from)
                    .map_err(KabiPayError::into_graphql)?,
            ),
            decision: Json(
                serde_json::to_value(review.decision)
                    .map_err(KabiPayError::from)
                    .map_err(KabiPayError::into_graphql)?,
            ),
        })
    }
}
