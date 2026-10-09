use crate::{repository as store, LoanActorScope, LoanModuleError, LoanResult};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) struct Posting<'a> {
    pub loan: Uuid,
    pub terms: Uuid,
    pub kind: &'a str,
    pub source_kind: &'a str,
    pub source: Uuid,
    pub revision: i64,
    pub date: NaiveDate,
    pub amount: Decimal,
    pub principal: Decimal,
    pub interest: Decimal,
    pub reversal: Option<Uuid>,
    pub evidence: serde_json::Value,
}
pub(crate) async fn post(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: Posting<'_>,
) -> LoanResult<Uuid> {
    if store::one(tx,"SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND source_kind=$3 AND source_id=$4",vec![actor.tenant_id.into(),input.loan.into(),input.source_kind.into(),input.source.into()]).await?.is_some(){return Err(LoanModuleError::SourceAlreadyPosted)}
    let row=store::one(tx,"SELECT COALESCE(MAX(sequence),0) AS sequence FROM loan_ledger_entry WHERE tenant_id=$1 AND loan_id=$2",vec![actor.tenant_id.into(),input.loan.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let sequence: i64 = row.try_get("", "sequence")?;
    let sequence = sequence
        .checked_add(1)
        .ok_or(LoanModuleError::InvalidCommand)?;
    let id = Uuid::new_v4();
    store::execute(tx,"INSERT INTO loan_posting(id,tenant_id,loan_id,kind,source_kind,source_id,source_revision,value_date,amount,principal_delta,interest_delta,terms_version_id,reversal_of,calculation_evidence,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",vec![id.into(),actor.tenant_id.into(),input.loan.into(),input.kind.into(),input.source_kind.into(),input.source.into(),input.revision.into(),input.date.into(),input.amount.into(),input.principal.into(),input.interest.into(),input.terms.into(),input.reversal.into(),input.evidence.into(),actor.user_id.into()]).await?;
    store::execute(tx,"INSERT INTO loan_ledger_entry(id,tenant_id,loan_id,posting_id,sequence,principal_delta,interest_delta) VALUES($1,$2,$3,$4,$5,$6,$7)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),input.loan.into(),id.into(),sequence.into(),input.principal.into(),input.interest.into()]).await?;
    if input.kind == "REPAYMENT"
        || (input.kind == "REVERSAL"
            && (input.principal > Decimal::ZERO || input.interest > Decimal::ZERO))
    {
        store::execute(tx,"INSERT INTO loan_allocation(id,tenant_id,loan_id,posting_id,principal,interest,direction) VALUES($1,$2,$3,$4,$5,$6,$7)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),input.loan.into(),id.into(),input.principal.abs().into(),input.interest.abs().into(),if input.kind=="REPAYMENT"{"RECOVER"}else{"REVERSE"}.into()]).await?;
    }
    Ok(id)
}
pub(crate) async fn net_funding(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
) -> LoanResult<Decimal> {
    let row=store::one(tx,"SELECT COALESCE(SUM(p.principal_delta),0)::text AS amount FROM loan_posting p LEFT JOIN loan_posting original ON original.tenant_id=p.tenant_id AND original.loan_id=p.loan_id AND original.id=p.reversal_of WHERE p.tenant_id=$1 AND p.loan_id=$2 AND (p.kind='DISBURSEMENT' OR (p.kind='REVERSAL' AND original.kind='DISBURSEMENT'))",vec![tenant.into(),loan.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    store::decimal(&row, "amount")
}
pub(crate) async fn bump_account(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
) -> LoanResult<i64> {
    let account = store::account(tx, tenant, loan).await?;
    let funding = net_funding(tx, tenant, loan).await?;
    let (principal, interest) = store::balances(tx, tenant, loan).await?;
    let state = if funding == account.approved_principal
        && principal == Decimal::ZERO
        && interest == Decimal::ZERO
    {
        "CLOSED"
    } else {
        "OPEN"
    };
    let funding_state = if funding == Decimal::ZERO {
        "UNFUNDED"
    } else if funding == account.approved_principal {
        "FUNDED"
    } else {
        "PARTIALLY_FUNDED"
    };
    let row=store::one(tx,"UPDATE loan_account SET version=version+1,state=$3,funding_state=$4 WHERE tenant_id=$1 AND id=$2 RETURNING version",vec![tenant.into(),loan.into(),state.into(),funding_state.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    Ok(row.try_get("", "version")?)
}
