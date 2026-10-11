use crate::{fnf::model::FinancialSnapshot, services::loan_recovery::PayslipLoanSnapshot};
use chrono::NaiveDate;
use kabipay_common::KabiPayResult;
use kabipay_loans::{FinancialIssue, LoanFinancialFacts, RecoverySourceFact};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

pub(super) struct IssuedRecovery {
    pub id: Uuid,
    kind: &'static str,
    revision: i64,
    date: NaiveDate,
    terms_version_id: Uuid,
    quote_fingerprint: String,
    principal: Decimal,
    interest: Decimal,
    posting_ids: Option<Vec<Uuid>>,
}
impl IssuedRecovery {
    pub fn matches(&self, fact: &RecoverySourceFact) -> bool {
        self.id == fact.source_id
            && self.kind == fact.source_kind
            && self.revision == fact.source_revision
            && self.date == fact.value_date
            && self.terms_version_id == fact.terms_version_id
            && fact.quote_fingerprint.as_deref() == Some(self.quote_fingerprint.as_str())
            && self.principal == fact.principal
            && self.interest == fact.interest
            && self
                .posting_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&fact.posting_id))
    }
}

pub(super) async fn read(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    facts: &LoanFinancialFacts,
) -> KabiPayResult<(Vec<IssuedRecovery>, Vec<FinancialIssue>)> {
    let mut expected = Vec::new();
    let mut issues = Vec::new();
    // JSON loan predicates also find issued allocations for which the entire Loan posting is absent.
    // Including source IDs finds damaged evidence whose JSON no longer identifies the loan.
    let source_ids: Vec<Uuid> = facts.recoveries.iter().map(|fact| fact.source_id).collect();
    let rows = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT e.cycle_id,e.source_revision,e.value_date,e.currency,e.recovery_total::text AS total,e.snapshot,c.status FROM payslip_loan_snapshot e JOIN payslip p ON p.tenant_id=e.tenant_id AND p.id=e.payslip_id AND p.employee_id=e.employee_id AND p.payroll_cycle_id=e.cycle_id JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE e.tenant_id=$1 AND e.employee_id=$2 AND (e.snapshot->'lines' @> jsonb_build_array(jsonb_build_object('loanId',$3::uuid::text)) OR e.cycle_id=ANY($4::uuid[]))",
        [tenant.into(),facts.employee_id.into(),facts.loan_id.into(),source_ids.clone().into()])).await?;
    for row in rows {
        let id: Uuid = row.try_get("", "cycle_id")?;
        let parsed = serde_json::from_value::<PayslipLoanSnapshot>(row.try_get("", "snapshot")?);
        let Ok(snapshot) = parsed else {
            issues.push(FinancialIssue::new("INVALID_PAYSLIP_EVIDENCE", Some(id)));
            continue;
        };
        let total = Decimal::from_str_exact(&snapshot.total).ok();
        let sum = snapshot.lines.iter().try_fold(Decimal::ZERO, |sum, line| {
            let principal = Decimal::from_str_exact(&line.principal_recovered).ok()?;
            let interest = Decimal::from_str_exact(&line.interest_recovered).ok()?;
            if principal < Decimal::ZERO || interest < Decimal::ZERO {
                return None;
            }
            sum.checked_add(principal)?.checked_add(interest)
        });
        if row.try_get::<String>("", "status")? != "PROCESSED"
            || snapshot.employee_id != facts.employee_id
            || snapshot.source_id != id
            || snapshot.source_revision != row.try_get::<i64>("", "source_revision")?
            || snapshot.value_date != row.try_get::<NaiveDate>("", "value_date")?
            || snapshot.currency != row.try_get::<String>("", "currency")?
            || total.is_none()
            || total != sum
            || total != Decimal::from_str_exact(&row.try_get::<String>("", "total")?).ok()
        {
            issues.push(FinancialIssue::new("INVALID_PAYSLIP_EVIDENCE", Some(id)));
            continue;
        }
        for line in snapshot
            .lines
            .iter()
            .filter(|line| line.loan_id == facts.loan_id)
        {
            let amounts = Decimal::from_str_exact(&line.principal_recovered)
                .ok()
                .zip(Decimal::from_str_exact(&line.interest_recovered).ok());
            match amounts {
                Some((principal, interest))
                    if principal > Decimal::ZERO || interest > Decimal::ZERO =>
                {
                    expected.push(IssuedRecovery {
                        id,
                        kind: "PAYROLL",
                        revision: snapshot.source_revision,
                        date: snapshot.value_date,
                        terms_version_id: line.terms_version_id,
                        quote_fingerprint: snapshot.quote_fingerprint.clone(),
                        principal,
                        interest,
                        posting_ids: Some(snapshot.posting_ids.clone()),
                    })
                }
                Some(_) => {}
                None => issues.push(FinancialIssue::new("INVALID_PAYSLIP_EVIDENCE", Some(id))),
            }
        }
    }
    let rows = tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT r.fnf_id,r.revision,r.value_date,r.snapshot,r.finalized_at,f.status FROM fnf_financial_review r JOIN fnf_settlement f ON f.tenant_id=r.tenant_id AND f.id=r.fnf_id WHERE r.tenant_id=$1 AND r.employee_id=$2 AND r.finalized_at IS NOT NULL AND (r.snapshot->'recoveryQuote'->'lines' @> jsonb_build_array(jsonb_build_object('loan_id',$3::uuid::text)) OR r.fnf_id=ANY($4::uuid[]))",
        [tenant.into(),facts.employee_id.into(),facts.loan_id.into(),source_ids.into()])).await?;
    for row in rows {
        let id: Uuid = row.try_get("", "fnf_id")?;
        let parsed = serde_json::from_value::<FinancialSnapshot>(row.try_get("", "snapshot")?);
        let Ok(snapshot) = parsed else {
            issues.push(FinancialIssue::new("INVALID_FNF_EVIDENCE", Some(id)));
            continue;
        };
        let Some(quote) = snapshot.recovery_quote else {
            issues.push(FinancialIssue::new("INVALID_FNF_EVIDENCE", Some(id)));
            continue;
        };
        let input = quote.input();
        if row.try_get::<String>("", "status")? != "PROCESSED"
            || input.source_id != id
            || input.source != kabipay_loans::RecoverySource::Fnf
            || input.employee_id != facts.employee_id
            || input.source_revision != row.try_get::<i64>("", "revision")?
            || input.value_date != row.try_get::<NaiveDate>("", "value_date")?
        {
            issues.push(FinancialIssue::new("INVALID_FNF_EVIDENCE", Some(id)));
            continue;
        }
        for line in quote.lines().iter().filter(|line| {
            line.loan_id == facts.loan_id
                && (line.principal > Decimal::ZERO || line.interest > Decimal::ZERO)
        }) {
            expected.push(IssuedRecovery {
                id,
                kind: "FNF",
                revision: input.source_revision,
                date: input.value_date,
                terms_version_id: line.terms_version_id,
                quote_fingerprint: quote.fingerprint().to_owned(),
                principal: line.principal,
                interest: line.interest,
                posting_ids: None,
            });
        }
    }
    Ok((expected, issues))
}
