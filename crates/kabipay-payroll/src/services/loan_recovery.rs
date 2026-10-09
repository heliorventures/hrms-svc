//! Payroll consumes the Loans API; it never reads or writes loan storage.
use super::automatic_payroll::PreparedEmployeePayroll;
use super::payroll_rules::CalculatedPeriod;
use kabipay_common::{
    client_data_scope::data_scope_from_claims,
    context::{ClientClaims, ScopeType},
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::payroll_cycle;
use kabipay_loans::{
    LoanActorScope, LoanModuleError, LoanPermission, RecoveryInput, RecoveryQuote, RecoverySource,
};
use kabipay_loans_domain::Currency;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewedLoanRecovery {
    pub quote: RecoveryQuote,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayslipLoanLine {
    pub loan_id: Uuid,
    pub terms_version_id: Uuid,
    pub policy_version_id: Uuid,
    pub schedule_version_id: Option<Uuid>,
    pub calculator_version: String,
    pub currency: String,
    pub principal_before: String,
    pub interest_before: String,
    pub accrued_interest: String,
    pub principal_recovered: String,
    pub interest_recovered: String,
    pub principal_after: String,
    pub interest_after: String,
    pub requested: String,
    pub deferred: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayslipLoanSnapshot {
    pub source_id: Uuid,
    pub source_revision: i64,
    pub employee_id: Uuid,
    pub value_date: chrono::NaiveDate,
    pub period_start: chrono::NaiveDate,
    pub currency: String,
    pub total: String,
    pub employee_revision: i64,
    pub quote_fingerprint: String,
    pub posting_ids: Vec<Uuid>,
    pub lines: Vec<PayslipLoanLine>,
}

fn financial_error(error: LoanModuleError) -> KabiPayError {
    match error {
        LoanModuleError::Authority(error) => error,
        LoanModuleError::Database(_) | LoanModuleError::Serialization(_) => {
            KabiPayError::Internal("Loan calculation or posting could not complete".into())
        }
        LoanModuleError::VersionConflict => {
            KabiPayError::Validation("Loan inputs changed; recalculate and review payroll".into())
        }
        LoanModuleError::RecoveryCapacity => KabiPayError::Validation(
            "Available salary does not satisfy the configured loan recovery policy".into(),
        ),
        LoanModuleError::PolicyUnavailable => KabiPayError::Validation(
            "Review Loans enablement, approved terms and company currency before payroll".into(),
        ),
        _ => KabiPayError::Validation(
            "Loan recovery requires review of its dates, terms, currency or financial history"
                .into(),
        ),
    }
}

pub(crate) fn actor(claims: &ClientClaims, tenant: Uuid) -> KabiPayResult<LoanActorScope> {
    let actor =
        LoanActorScope::from_verified_claims(claims, tenant, LoanPermission::PayrollRecovery)
            .map_err(financial_error)?;
    if data_scope_from_claims(Some(claims), "payroll:manage")? != ScopeType::All {
        return Err(KabiPayError::Forbidden(
            "Payroll finalization requires ALL scope".into(),
        ));
    }
    Ok(actor)
}

pub(crate) async fn lock(tx: &DatabaseTransaction, tenant: Uuid) -> KabiPayResult<()> {
    tx.execute(Statement::from_string(
        DbBackend::Postgres,
        "SET LOCAL lock_timeout='15s'",
    ))
    .await?;
    kabipay_loans::lock_recovery_inputs(tx, tenant)
        .await
        .map_err(financial_error)
}

pub async fn fingerprint(tx: &DatabaseTransaction, tenant: Uuid) -> KabiPayResult<String> {
    let payroll = super::payroll_fingerprint::fingerprint(tx, tenant).await?;
    let loans = kabipay_loans::payroll_loan_state(tx, tenant)
        .await
        .map_err(financial_error)?;
    kabipay_loans::command_hash(&("payroll-with-loans-v1", payroll, loans.fingerprint))
        .map_err(financial_error)
}

pub async fn prepare(
    tx: &DatabaseTransaction,
    claims: &ClientClaims,
    cycle: &payroll_cycle::Model,
    revision: i32,
    employee: Uuid,
    prepared: &mut PreparedEmployeePayroll,
) -> KabiPayResult<()> {
    let actor = actor(claims, cycle.tenant_id)?;
    let state = kabipay_loans::payroll_loan_state(tx, cycle.tenant_id)
        .await
        .map_err(financial_error)?;
    if !state.enabled {
        return Ok(());
    }
    let value_date = cycle.payment_date.ok_or_else(|| {
        KabiPayError::Validation("Set and review the payroll payment date for loan recovery".into())
    })?;
    let (period_start, _) =
        kabipay_tax::domain::tax_year::month_bounds(cycle.year, cycle.month as u32)?;
    if value_date < period_start {
        return Err(KabiPayError::Validation(
            "Loan recovery payment date precedes the salary period".into(),
        ));
    }
    // Current payroll uses two decimal places. Other loan precision cannot be netted silently.
    let currency = Currency {
        code: state.currency.ok_or_else(|| {
            KabiPayError::Validation("Configure company currency before loan recovery".into())
        })?,
        minor_units: 2,
    };
    if currency.code != "INR" {
        return Err(KabiPayError::Validation("Current payroll and payslip currency is INR; a different loan or company currency cannot be recovered through this payroll engine".into()));
    }
    let prior=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COALESCE(SUM(COALESCE((s.statement->>'remaining_payable')::numeric,p.net_salary)+COALESCE(e.recovery_total,0)),0)::text AS eligible FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id LEFT JOIN payslip_statement s ON s.tenant_id=p.tenant_id AND s.payslip_id=p.id LEFT JOIN payslip_loan_snapshot e ON e.tenant_id=p.tenant_id AND e.payslip_id=p.id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND c.year=$3 AND c.month=$4 AND c.status='PROCESSED' AND c.id<>$5",
        [cycle.tenant_id.into(),employee.into(),cycle.year.into(),cycle.month.into(),cycle.id.into()])).await?.ok_or_else(||KabiPayError::Internal("Prior salary capacity is unavailable".into()))?;
    let prior = Decimal::from_str_exact(&prior.try_get::<String>("", "eligible")?)
        .map_err(|_| KabiPayError::Validation("Prior salary capacity is invalid".into()))?;
    let available = prepared.calculation.remaining_payable;
    let eligible = available.checked_add(prior).ok_or_else(|| {
        KabiPayError::Validation("Salary capacity is outside the supported range".into())
    })?;
    let input = RecoveryInput {
        employee_id: employee,
        source: RecoverySource::Payroll,
        source_id: cycle.id,
        source_revision: i64::from(revision),
        period_start,
        value_date,
        eligible_net: eligible,
        available_net: available,
        currency,
    };
    let quote = kabipay_loans::prepare_recovery_quote(tx, &actor, &input)
        .await
        .map_err(financial_error)?;
    apply_recovery(&mut prepared.calculation, quote.total())?;
    prepared.loan_recovery = Some(ReviewedLoanRecovery { quote });
    Ok(())
}

pub(crate) fn validate_calculation(
    prepared: &PreparedEmployeePayroll,
    cycle: &payroll_cycle::Model,
    employee: Uuid,
    revision: i32,
    enabled: bool,
) -> KabiPayResult<()> {
    if enabled != prepared.loan_recovery.is_some() {
        return Err(KabiPayError::Validation(
            "Recalculate payroll with the current Loans configuration".into(),
        ));
    }
    if let Some(reviewed) = &prepared.loan_recovery {
        let input = reviewed.quote.input();
        if input.source != RecoverySource::Payroll
            || input.source_id != cycle.id
            || input.source_revision != i64::from(revision)
            || input.employee_id != employee
            || Some(input.value_date) != cycle.payment_date
        {
            return Err(KabiPayError::Validation(
                "Loan review does not belong to this payroll draft".into(),
            ));
        }
        let mut expected = super::payroll_rules::calculate_period(&prepared.input)?;
        if expected.remaining_payable != input.available_net {
            return Err(KabiPayError::Validation(
                "Reviewed salary capacity changed".into(),
            ));
        }
        apply_recovery(&mut expected, reviewed.quote.total())?;
        let expected = serde_json::to_value(expected)
            .map_err(|_| KabiPayError::Internal("Payroll evidence serialization failed".into()))?;
        let actual = serde_json::to_value(&prepared.calculation)
            .map_err(|_| KabiPayError::Internal("Payroll evidence serialization failed".into()))?;
        if expected != actual {
            return Err(KabiPayError::Validation(
                "Reviewed payroll deductions do not match the loan quote".into(),
            ));
        }
    }
    Ok(())
}

pub async fn post(
    tx: &DatabaseTransaction,
    claims: &ClientClaims,
    reviewed: &ReviewedLoanRecovery,
) -> KabiPayResult<PayslipLoanSnapshot> {
    let actor = actor(claims, claims.tenant_id)?;
    let quote = &reviewed.quote;
    let posted = kabipay_loans::post_payroll_recoveries(
        tx,
        &kabipay_loans::PostRecoveriesInput {
            actor: &actor,
            quote,
        },
    )
    .await
    .map_err(financial_error)?;
    let lines = quote
        .lines()
        .iter()
        .map(|line| {
            let paid = line.principal + line.interest;
            PayslipLoanLine {
                loan_id: line.loan_id,
                terms_version_id: line.terms_version_id,
                policy_version_id: line.policy_version_id,
                schedule_version_id: line.schedule_version_id,
                calculator_version: line.calculator_version.clone(),
                currency: line.currency.clone(),
                principal_before: line.principal_before.to_string(),
                interest_before: line.interest_before.to_string(),
                accrued_interest: line.accrued_interest.to_string(),
                principal_recovered: line.principal.to_string(),
                interest_recovered: line.interest.to_string(),
                principal_after: (line.principal_before - line.principal).to_string(),
                interest_after: (line.interest_before
                    + if paid > Decimal::ZERO {
                        line.accrued_interest
                    } else {
                        Decimal::ZERO
                    }
                    - line.interest)
                    .to_string(),
                requested: line.requested.to_string(),
                deferred: line.deferred.to_string(),
            }
        })
        .collect();
    let input = quote.input();
    Ok(PayslipLoanSnapshot {
        source_id: input.source_id,
        source_revision: input.source_revision,
        employee_id: input.employee_id,
        value_date: input.value_date,
        period_start: input.period_start,
        currency: input.currency.code.clone(),
        total: posted.total.to_string(),
        employee_revision: posted.employee_revision,
        quote_fingerprint: quote.fingerprint().into(),
        posting_ids: posted.posting_ids,
        lines,
    })
}

pub(crate) async fn persist(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    slip: Uuid,
    snapshot: &PayslipLoanSnapshot,
) -> KabiPayResult<()> {
    let value = serde_json::to_value(snapshot)
        .map_err(|_| KabiPayError::Internal("Loan payslip evidence serialization failed".into()))?;
    let total = Decimal::from_str_exact(&snapshot.total)
        .map_err(|_| KabiPayError::Internal("Loan payslip total is invalid".into()))?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO payslip_loan_snapshot(payslip_id,tenant_id,employee_id,cycle_id,source_revision,value_date,currency,recovery_total,snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        [slip.into(),tenant.into(),snapshot.employee_id.into(),snapshot.source_id.into(),snapshot.source_revision.into(),snapshot.value_date.into(),snapshot.currency.clone().into(),total.into(),value.into()])).await?;
    Ok(())
}

pub fn apply_recovery(value: &mut CalculatedPeriod, recovery: Decimal) -> KabiPayResult<()> {
    if recovery < Decimal::ZERO
        || recovery > value.remaining_payable
        || value.loan_recovery != Decimal::ZERO
    {
        return Err(KabiPayError::Validation(
            "Loan recovery exceeds available salary or was already applied".into(),
        ));
    }
    let deductions = value.total_deductions.checked_add(recovery);
    let net = value.net_earned.checked_sub(recovery);
    let remaining = value.remaining_payable.checked_sub(recovery);
    let (Some(deductions), Some(net), Some(remaining)) = (deductions, net, remaining) else {
        return Err(KabiPayError::Validation(
            "Loan recovery amount is outside the supported range".into(),
        ));
    };
    value.total_deductions = deductions;
    value.net_earned = net;
    value.remaining_payable = remaining;
    value.loan_recovery = recovery;
    Ok(())
}
