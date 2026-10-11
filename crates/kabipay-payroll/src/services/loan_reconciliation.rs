//! Payroll/FNF verify their issued evidence; Loans alone owns its ledger and audit storage.
mod sources;
use kabipay_common::KabiPayResult;
use kabipay_loans::{FinancialIssue, LoanFinancialReport};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use uuid::Uuid;

pub async fn reconcile_account(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
    actor: Option<Uuid>,
) -> KabiPayResult<LoanFinancialReport> {
    let facts = kabipay_loans::inspect_loan_financials(tx, tenant, loan)
        .await
        .map_err(crate::fnf::coordination::financial_error)?;
    let (expected, mut issues) = sources::read(tx, tenant, &facts).await?;
    let mut verified = 0;
    for fact in &facts.recoveries {
        let matches: Vec<_> = expected
            .iter()
            .filter(|source| source.matches(fact))
            .collect();
        if matches.len() == 1
            && facts
                .recoveries
                .iter()
                .filter(|other| matches[0].matches(other))
                .count()
                == 1
        {
            verified += 1;
        } else {
            issues.push(FinancialIssue::new(
                "ISSUED_SOURCE_MISMATCH",
                Some(fact.posting_id),
            ));
        }
    }
    for source in &expected {
        if facts
            .recoveries
            .iter()
            .filter(|fact| source.matches(fact))
            .count()
            != 1
        {
            issues.push(FinancialIssue::new(
                "ISSUED_ALLOCATION_MISMATCH",
                Some(source.id),
            ));
        }
    }
    kabipay_loans::persist_loan_audit(tx, tenant, facts, issues, verified, actor)
        .await
        .map_err(crate::fnf::coordination::financial_error)
}

/// Bounded private consumer. Failed transactions retain events for the next sweep.
pub async fn reconcile_due(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<()> {
    let tx = db.begin().await?;
    let loans = kabipay_loans::due_financial_audits(&tx, tenant, 10)
        .await
        .map_err(crate::fnf::coordination::financial_error)?;
    for loan in loans {
        reconcile_account(&tx, tenant, loan, None).await?;
    }
    kabipay_loans::acknowledge_financial_events(&tx, tenant, 100)
        .await
        .map_err(crate::fnf::coordination::financial_error)?;
    tx.commit().await?;
    Ok(())
}
