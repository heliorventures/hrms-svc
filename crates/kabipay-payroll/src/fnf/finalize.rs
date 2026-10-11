use super::{coordination::financial_error, draft, model::FinancialReview, source};
use kabipay_common::{
    context::ClientClaims, finance_contract::ExitSubject, KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

pub async fn finalize(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    subject: &ExitSubject,
    expected_revision: i64,
    expected_fingerprint: &str,
    company_today: chrono::NaiveDate,
) -> KabiPayResult<FinancialReview> {
    let actor = super::review::actor(claims, tenant)?;
    let tx = db.begin().await?;
    crate::services::loan_recovery::lock(&tx, tenant).await?;
    let row = source::find(&tx, tenant, subject.separation_id, true)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "fnf_settlement",
            id: subject.separation_id.to_string(),
        })?;
    let reviewed = draft::load(&tx, tenant, row.id, true)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation(
                "Calculate and review complete FNF financial sources before finalization".into(),
            )
        })?;
    if reviewed.revision != expected_revision || reviewed.fingerprint != expected_fingerprint {
        return Err(KabiPayError::Conflict(
            "FNF review changed; review its current figures before finalization".into(),
        ));
    }
    if reviewed.finalized && row.status == "PROCESSED" {
        tx.commit().await?;
        return Ok(reviewed);
    }
    if row.status != "DRAFT" || reviewed.finalized {
        return Err(KabiPayError::Conflict(
            "Issued FNF record is immutable or requires reconciliation".into(),
        ));
    }
    super::integrity::verify(&tx, tenant, &row, &reviewed.snapshot).await?;
    if reviewed.snapshot.input.value_date > company_today {
        return Err(KabiPayError::Validation(format!(
            "FNF can be finalized on or after {} in company time",
            reviewed.snapshot.input.value_date
        )));
    }
    let fresh = super::review::calculate(
        &tx,
        tenant,
        claims,
        subject,
        row.id,
        reviewed.revision,
        &reviewed.snapshot.input,
    )
    .await?;
    if kabipay_loans::command_hash(&fresh).map_err(financial_error)? != reviewed.fingerprint {
        return Err(KabiPayError::Conflict(
            "Loan, salary, separation or FNF inputs changed; calculate and review FNF again".into(),
        ));
    }
    if let Some(quote) = &fresh.recovery_quote {
        kabipay_loans::post_fnf_recoveries(
            &tx,
            &kabipay_loans::PostRecoveriesInput {
                actor: &actor,
                quote,
            },
        )
        .await
        .map_err(financial_error)?;
    }
    kabipay_loans::confirm_fnf_arrangements(&tx, &actor, subject.employee_id, subject.separation_id,
        row.id, subject.last_working_date, fresh.input.value_date, &fresh.input.loan_decision)
        .await.map_err(financial_error)?;
    // Seal evidence and mark the settlement in the same financial transaction as Loan set-off.
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE fnf_financial_review SET finalized_at=NOW(),updated_at=NOW() WHERE tenant_id=$1 AND fnf_id=$2 AND finalized_at IS NULL",
        [tenant.into(),row.id.into()])).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE fnf_settlement SET status='PROCESSED',processed_at=NOW(),processed_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND status='DRAFT'",
        [tenant.into(),row.id.into(),claims.sub.into()])).await?;
    tx.commit().await?;
    Ok(FinancialReview {
        finalized: true,
        ..reviewed
    })
}
