use super::model::FinancialReview;
use async_graphql::{InputObject, Json, SimpleObject, ID};
use chrono::{DateTime, Utc};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::fnf_settlement;
use serde_json::Value;

#[derive(SimpleObject)]
pub struct FinanceOwner { pub id: uuid::Uuid, pub name: String }
#[derive(SimpleObject)]
pub struct FinanceOwnerPageInfo { pub end_cursor: Option<String>, pub has_next_page: bool }
#[derive(SimpleObject)]
pub struct FinanceOwnerConnection { pub nodes: Vec<FinanceOwner>, pub page_info: FinanceOwnerPageInfo }

#[derive(SimpleObject)]
#[graphql(name = "FnfSettlement")]
pub struct FnfSettlementDto {
    pub id: ID,
    pub tenant_id: ID,
    pub separation_id: ID,
    pub leave_encashment: Option<String>,
    pub gratuity_amount: Option<String>,
    pub bonus_payable: Option<String>,
    pub recovery_amount: Option<String>,
    pub net_payable: Option<String>,
    pub status: String,
    pub processed_at: Option<DateTime<Utc>>,
    pub processed_by: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub financials: Option<Json<Value>>,
}
impl FnfSettlementDto {
    pub(super) fn new(row: fnf_settlement::Model, review: Option<&FinancialReview>) -> Self {
        Self {
            id: row.id.to_string().into(),
            tenant_id: row.tenant_id.to_string().into(),
            separation_id: row.separation_id.to_string().into(),
            leave_encashment: row.leave_encashment.map(|v| v.to_string()),
            gratuity_amount: row.gratuity_amount.map(|v| v.to_string()),
            bonus_payable: row.bonus_payable.map(|v| v.to_string()),
            recovery_amount: row.recovery_amount.map(|v| v.to_string()),
            net_payable: row.net_payable.map(|v| v.to_string()),
            status: row.status,
            processed_at: row.processed_at,
            processed_by: row.processed_by.map(|v| v.to_string().into()),
            created_at: row.created_at,
            updated_at: row.updated_at,
            financials: review.map(|r| Json(evidence(r))),
        }
    }
}

fn evidence(review: &FinancialReview) -> Value {
    let snapshot = &review.snapshot;
    let lines=snapshot.exposure.loans.iter().map(|loan| {
        let recovery=snapshot.recovery_quote.as_ref().and_then(|quote|quote.lines().iter().find(|line|line.loan_id==loan.loan_id));
        let principal=recovery.map_or(rust_decimal::Decimal::ZERO,|line|line.principal);
        let interest=recovery.map_or(rust_decimal::Decimal::ZERO,|line|line.interest);
        serde_json::json!({"loanId":loan.loan_id,"loanNumber":loan.loan_number,"opening":loan.forecast_payoff.to_string(),
            "principalRecovered":principal.to_string(),"interestRecovered":interest.to_string(),"totalRecovered":(principal+interest).to_string(),
            "closing":(loan.forecast_payoff-principal-interest).to_string(),"unappliedCredit":loan.unapplied_credit.to_string()})
    }).collect::<Vec<_>>();
    let sources=snapshot.input.sources.iter().map(|source| serde_json::json!({"kind":source.kind.wire(),"sourceId":source.source_id,
        "amount":source.amount.to_string(),"alreadyPaid":source.already_paid.to_string(),"otherRecovery":source.other_recovery.to_string(),"eligibleForLoan":source.eligible_for_loan})).collect::<Vec<_>>();
    serde_json::json!({"currency":snapshot.input.currency.code,"minorUnits":snapshot.input.currency.minor_units,
        "valueDate":snapshot.input.value_date,"availablePayout":snapshot.available_payout.to_string(),"otherRecoveries":snapshot.other_recoveries.to_string(),
        "loanRecovered":snapshot.loan_recovered.to_string(),"employeePayout":snapshot.employee_payout.to_string(),
        "residualLoanReceivable":snapshot.residual_loan_receivable.to_string(),"sources":sources,"loans":lines,"finalized":review.finalized})
}

#[derive(InputObject)]
pub struct UpsertFnfSettlementInput {
    pub separation_id: ID,
    pub leave_encashment: Option<String>,
    pub gratuity_amount: Option<String>,
    pub bonus_payable: Option<String>,
    pub recovery_amount: Option<String>,
}

#[derive(InputObject)]
pub struct SaveFnfFinancialReviewInput {
    pub separation_id: uuid::Uuid,
    pub input: Json<Value>,
    pub expected_revision: Option<i64>,
    pub expected_fingerprint: Option<String>,
}

#[derive(SimpleObject)]
pub struct FnfFinancialPreparation {
    pub settlement: Option<FnfSettlementDto>,
    pub revision: Option<i64>,
    pub fingerprint: Option<String>,
    pub input: Option<Json<Value>>,
    pub legacy_figures_fingerprint: Option<String>,
}
