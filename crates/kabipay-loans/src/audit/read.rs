use super::{FinancialIssue, LoanFinancialFacts, RecoverySourceFact};
use crate::{repository as store, LoanModuleError, LoanResult};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub async fn inspect_loan_financials(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
) -> LoanResult<LoanFinancialFacts> {
    if tenant.is_nil() {
        return Err(LoanModuleError::Forbidden);
    }
    store::synchronize(tx, tenant).await?;
    let account = store::account(tx, tenant, loan).await?;
    let mismatches = store::all(tx, r#"
        SELECT 'POSTING_LEDGER_MISMATCH' AS code,p.id AS reference FROM loan_posting p
        LEFT JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id
        WHERE p.tenant_id=$1 AND p.loan_id=$2 AND (l.id IS NULL OR l.principal_delta<>p.principal_delta OR l.interest_delta<>p.interest_delta)
        UNION ALL
        SELECT 'REPAYMENT_ALLOCATION_MISMATCH',p.id FROM loan_posting p
        LEFT JOIN loan_allocation a ON a.tenant_id=p.tenant_id AND a.loan_id=p.loan_id AND a.posting_id=p.id
        WHERE p.tenant_id=$1 AND p.loan_id=$2 AND p.kind='REPAYMENT'
          AND (a.id IS NULL OR a.principal<>-p.principal_delta OR a.interest<>-p.interest_delta OR a.direction<>'RECOVER')
        UNION ALL
        SELECT 'DISBURSEMENT_SOURCE_MISMATCH',d.id FROM loan_disbursement d
        LEFT JOIN loan_posting p ON p.tenant_id=d.tenant_id AND p.loan_id=d.loan_id AND p.source_id=d.id AND p.source_kind='EXTERNAL_DISBURSEMENT'
        WHERE d.tenant_id=$1 AND d.loan_id=$2 AND (p.id IS NULL OR p.amount<>d.amount OR p.principal_delta<>d.amount)
        UNION ALL
        SELECT 'RECEIPT_SOURCE_MISMATCH',r.id FROM loan_receipt r
        WHERE r.tenant_id=$1 AND r.loan_id=$2 AND r.amount <>
          COALESCE((SELECT SUM(p.amount) FROM loan_posting p WHERE p.tenant_id=r.tenant_id AND p.loan_id=r.loan_id AND p.source_id=r.id AND p.source_kind='EXTERNAL_RECEIPT'),0)
          + COALESCE((SELECT SUM(c.amount) FROM loan_unapplied_credit c WHERE c.tenant_id=r.tenant_id AND c.loan_id=r.loan_id AND c.receipt_id=r.id),0)
    "#, vec![tenant.into(), loan.into()]).await?;
    let mut issues = mismatches
        .into_iter()
        .map(|row| {
            Ok(FinancialIssue {
                code: row.try_get("", "code")?,
                reference_id: Some(row.try_get("", "reference")?),
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    let (principal, interest) = store::balances(tx, tenant, loan).await?;
    let funding = crate::posting::net_funding(tx, tenant, loan).await?;
    if principal.is_sign_negative()
        || interest.is_sign_negative()
        || funding.is_sign_negative()
        || funding > account.approved_principal
    {
        issues.push(FinancialIssue::new("ACCOUNT_BALANCE_MISMATCH", Some(loan)));
    }
    let closed = principal.is_zero()
        && interest.is_zero()
        && (account.funding_state == "CANCELLED" || funding == account.approved_principal);
    if (account.state == "CLOSED") != closed {
        issues.push(FinancialIssue::new("ACCOUNT_STATE_MISMATCH", Some(loan)));
    }
    let rows = store::all(tx,
        "SELECT id,source_kind,source_id,source_revision,value_date,terms_version_id,calculation_evidence->>'quoteFingerprint' AS quote_fingerprint,(-principal_delta)::text AS principal,(-interest_delta)::text AS interest FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND kind='REPAYMENT' AND source_kind IN ('PAYROLL','FNF') ORDER BY created_at,id",
        vec![tenant.into(), loan.into()]).await?;
    let recoveries = rows
        .into_iter()
        .map(|row| {
            Ok(RecoverySourceFact {
                posting_id: row.try_get("", "id")?,
                source_kind: row.try_get("", "source_kind")?,
                source_id: row.try_get("", "source_id")?,
                source_revision: row.try_get("", "source_revision")?,
                value_date: row.try_get("", "value_date")?,
                terms_version_id: row.try_get("", "terms_version_id")?,
                quote_fingerprint: row.try_get("", "quote_fingerprint")?,
                principal: store::decimal(&row, "principal")?,
                interest: store::decimal(&row, "interest")?,
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    Ok(LoanFinancialFacts {
        loan_id: loan,
        employee_id: account.employee_id,
        account_version: account.version,
        checked_at: chrono::Utc::now(),
        issues,
        recoveries,
    })
}
