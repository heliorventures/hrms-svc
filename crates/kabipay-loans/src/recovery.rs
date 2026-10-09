//! Private financial coordination DTOs exclude purposes, employee notes and attachments.
use crate::{
    access, accrual, arrangement, lifecycle,
    posting::{self, Posting},
    repository as store, LoanActorScope, LoanModuleError, LoanPermission, LoanResult,
};
use chrono::{Datelike, NaiveDate};
use kabipay_loans_domain::{
    allocate_repayment, AllocationInput, Currency, LoanPolicy, LoanTerms, RecoveryPriority,
    ShortSalaryPolicy,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoverySource {
    Payroll,
    Fnf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryInput {
    pub employee_id: Uuid,
    pub source: RecoverySource,
    pub source_id: Uuid,
    pub source_revision: i64,
    pub period_start: NaiveDate,
    pub value_date: NaiveDate,
    /// Payroll: reviewed cumulative eligible pay for this employee and salary period,
    /// before loan recovery. FNF: reviewed eligible set-off pool. Supplied by the owning
    /// financial module from persisted sources, never by a browser.
    pub eligible_net: Decimal,
    /// Cash remaining in this specific run after non-loan deductions and prior payments.
    pub available_net: Decimal,
    pub currency: Currency,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryLine {
    pub loan_id: Uuid,
    pub terms_version_id: Uuid,
    pub policy_version_id: Uuid,
    pub schedule_version_id: Option<Uuid>,
    pub calculator_version: String,
    pub account_version: i64,
    pub currency: String,
    pub principal_before: Decimal,
    pub interest_before: Decimal,
    pub accrued_interest: Decimal,
    pub principal: Decimal,
    pub interest: Decimal,
    pub requested: Decimal,
    pub deferred: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryQuote {
    input: RecoveryInput,
    employee_revision: i64,
    lines: Vec<RecoveryLine>,
    total: Decimal,
    fingerprint: String,
}
impl RecoveryQuote {
    pub fn input(&self) -> &RecoveryInput {
        &self.input
    }
    pub fn employee_revision(&self) -> i64 {
        self.employee_revision
    }
    pub fn lines(&self) -> &[RecoveryLine] {
        &self.lines
    }
    pub fn total(&self) -> Decimal {
        self.total
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct PostingResult {
    pub posting_ids: Vec<Uuid>,
    pub employee_revision: i64,
    pub total: Decimal,
}
pub struct PostRecoveriesInput<'a> {
    pub actor: &'a LoanActorScope,
    pub quote: &'a RecoveryQuote,
}

pub async fn lock_recovery_inputs(tx: &DatabaseTransaction, tenant: Uuid) -> LoanResult<()> {
    if tenant.is_nil() {
        return Err(LoanModuleError::Forbidden);
    }
    store::synchronize(tx, tenant).await
}
async fn authorized(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: &RecoveryInput,
) -> LoanResult<()> {
    let expected = match input.source {
        RecoverySource::Payroll => LoanPermission::PayrollRecovery,
        RecoverySource::Fnf => LoanPermission::FnfRecovery,
    };
    if actor.permission != expected
        || input.source_id.is_nil()
        || input.source_revision <= 0
        || input.period_start.day() != 1
    {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    if input.source == RecoverySource::Payroll {
        kabipay_common::entitlements::Entitlements::load(tx, actor.tenant_id)
            .await?
            .require("PAYROLL")?;
    }
    access::employee(tx, actor, input.employee_id).await?;
    input.currency.validate_amount(input.eligible_net)?;
    input.currency.validate_amount(input.available_net)?;
    if input.available_net > input.eligible_net {
        return Err(LoanModuleError::InvalidCommand);
    }
    Ok(())
}
struct Candidate {
    line: RecoveryLine,
    terms: LoanTerms,
    policy: LoanPolicy,
    created_at: chrono::DateTime<chrono::Utc>,
}
pub async fn prepare_recovery_quote(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    input: &RecoveryInput,
) -> LoanResult<RecoveryQuote> {
    authorized(tx, actor, input).await?;
    lock_recovery_inputs(tx, actor.tenant_id).await?;
    let revision = store::revision(tx, actor.tenant_id, input.employee_id).await?;
    let rows=store::all(tx,"SELECT id FROM loan_account WHERE tenant_id=$1 AND employee_id=$2 AND state='OPEN' ORDER BY id FOR UPDATE",vec![actor.tenant_id.into(),input.employee_id.into()]).await?;
    let mut candidates = vec![];
    let mut priority = None;
    let mut budget = input.eligible_net;
    for row in rows {
        let loan: Uuid = row.try_get("", "id")?;
        let account = store::account(tx, actor.tenant_id, loan).await?;
        let version = store::terms(tx, &account).await?;
        let terms: LoanTerms = serde_json::from_value(version.terms)?;
        if terms.currency != input.currency {
            return Err(LoanModuleError::PolicyUnavailable);
        }
        let policy = store::policy(tx, actor.tenant_id, version.policy_version_id).await?;
        let policy: LoanPolicy = serde_json::from_value(policy.rules)?;
        policy.validate_terms(&terms)?;
        if priority.is_some_and(|value| value != policy.recovery.priority) {
            return Err(LoanModuleError::PriorityConflict);
        }
        priority = Some(policy.recovery.priority);
        let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
        let earned = accrual::calculate(
            tx,
            actor.tenant_id,
            loan,
            &terms,
            principal,
            account.accrued_through,
            input.value_date,
            account.rounding_carry,
        )
        .await?;
        let due_interest = interest
            .checked_add(earned.amount)
            .ok_or(LoanModuleError::InvalidCommand)?;
        let payable = principal
            .checked_add(due_interest)
            .ok_or(LoanModuleError::InvalidCommand)?;
        let mut schedule_version_id = None;
        let requested = match input.source {
            RecoverySource::Fnf => {
                if policy.exit_recovery.allow_fnf {
                    payable
                } else {
                    Decimal::ZERO
                }
            }
            RecoverySource::Payroll => {
                let schedule=store::one(tx,"SELECT s.id,s.monthly_amount::text AS amount,s.recovery_mode FROM loan_schedule_version s WHERE s.tenant_id=$1 AND s.loan_id=$2 AND s.effective_from<=$3 ORDER BY s.version DESC LIMIT 1",vec![actor.tenant_id.into(),loan.into(),input.value_date.into()]).await?;
                if let Some(schedule) = schedule {
                    let mode: String = schedule.try_get("", "recovery_mode")?;
                    let schedule_id: Uuid = schedule.try_get("", "id")?;
                    schedule_version_id = Some(schedule_id);
                    let due=store::one(tx,"SELECT id FROM loan_schedule_item WHERE tenant_id=$1 AND loan_id=$2 AND schedule_version_id=$3 AND date_trunc('month',due_date)::date=$4 AND due_date<=$5 LIMIT 1",vec![actor.tenant_id.into(),loan.into(),schedule_id.into(),input.period_start.into(),input.value_date.into()]).await?.is_some();
                    if mode == "EXTERNAL" || !due {
                        Decimal::ZERO
                    } else {
                        let override_ = arrangement::overrides(tx, actor.tenant_id, loan)
                            .await?
                            .into_iter()
                            .find(|value| value.period == input.period_start);
                        let amount = match override_ {
                            Some(value) => value.amount.unwrap_or(Decimal::ZERO),
                            None => store::decimal(&schedule, "amount")?,
                        };
                        let row=store::one(tx,"SELECT COALESCE(-SUM(p.principal_delta+p.interest_delta),0)::text AS recovered FROM loan_posting p LEFT JOIN loan_posting original ON original.tenant_id=p.tenant_id AND original.loan_id=p.loan_id AND original.id=p.reversal_of WHERE p.tenant_id=$1 AND p.loan_id=$2 AND (CASE WHEN p.kind='REVERSAL' THEN original.calculation_evidence ELSE p.calculation_evidence END)->>'periodStart'=$3::date::text AND (p.source_kind='PAYROLL' OR (p.kind='REVERSAL' AND original.source_kind='PAYROLL'))",vec![actor.tenant_id.into(),loan.into(),input.period_start.into()]).await?.ok_or(LoanModuleError::NotFound)?;
                        amount
                            .checked_sub(store::decimal(&row, "recovered")?)
                            .ok_or(LoanModuleError::InvalidCommand)?
                            .max(Decimal::ZERO)
                            .min(payable)
                    }
                } else {
                    Decimal::ZERO
                }
            }
        };
        if input.source == RecoverySource::Payroll && requested > Decimal::ZERO {
            let protected = input
                .eligible_net
                .checked_sub(policy.recovery.minimum_net_pay)
                .ok_or(LoanModuleError::InvalidCommand)?
                .max(Decimal::ZERO);
            let percent = input
                .eligible_net
                .checked_mul(policy.recovery.maximum_net_pay_percentage)
                .and_then(|v| v.checked_div(Decimal::from(100)))
                .ok_or(LoanModuleError::InvalidCommand)?
                .round_dp_with_strategy(
                    input.currency.minor_units,
                    rust_decimal::RoundingStrategy::ToZero,
                );
            budget = budget.min(protected).min(percent);
        }
        candidates.push(Candidate {
            line: RecoveryLine {
                loan_id: loan,
                terms_version_id: version.id,
                policy_version_id: version.policy_version_id,
                schedule_version_id,
                calculator_version: terms.calculator_version.clone(),
                account_version: account.version,
                currency: terms.currency.code.clone(),
                principal_before: principal,
                interest_before: interest,
                accrued_interest: earned.amount,
                principal: Decimal::ZERO,
                interest: Decimal::ZERO,
                requested,
                deferred: Decimal::ZERO,
            },
            terms,
            policy,
            created_at: account.created_at,
        });
    }
    if input.source == RecoverySource::Payroll {
        // Protection is one employee/period budget. Include policy protections from loans
        // already paid off in the period, then subtract recovery in earlier salary runs.
        let prior=store::all(tx,"SELECT DISTINCT policy.rules,a.currency FROM loan_posting p JOIN loan_account a ON a.tenant_id=p.tenant_id AND a.id=p.loan_id JOIN loan_terms_version t ON t.tenant_id=p.tenant_id AND t.loan_id=p.loan_id AND t.id=p.terms_version_id JOIN loan_policy_version policy ON policy.tenant_id=t.tenant_id AND policy.id=t.policy_version_id WHERE p.tenant_id=$1 AND a.employee_id=$2 AND p.source_kind='PAYROLL' AND p.calculation_evidence->>'periodStart'=$3::date::text",vec![actor.tenant_id.into(),input.employee_id.into(),input.period_start.into()]).await?;
        for row in prior {
            if row.try_get::<String>("", "currency")? != input.currency.code {
                return Err(LoanModuleError::PolicyUnavailable);
            }
            let policy: LoanPolicy = serde_json::from_value(row.try_get("", "rules")?)?;
            policy.validate(&input.currency)?;
            let protected = input
                .eligible_net
                .checked_sub(policy.recovery.minimum_net_pay)
                .ok_or(LoanModuleError::InvalidCommand)?
                .max(Decimal::ZERO);
            let percent = input
                .eligible_net
                .checked_mul(policy.recovery.maximum_net_pay_percentage)
                .and_then(|v| v.checked_div(Decimal::from(100)))
                .ok_or(LoanModuleError::InvalidCommand)?
                .round_dp_with_strategy(
                    input.currency.minor_units,
                    rust_decimal::RoundingStrategy::ToZero,
                );
            budget = budget.min(protected).min(percent);
        }
        let row=store::one(tx,"SELECT COALESCE(-SUM(p.principal_delta+p.interest_delta),0)::text AS recovered FROM loan_posting p JOIN loan_account a ON a.tenant_id=p.tenant_id AND a.id=p.loan_id LEFT JOIN loan_posting original ON original.tenant_id=p.tenant_id AND original.loan_id=p.loan_id AND original.id=p.reversal_of WHERE p.tenant_id=$1 AND a.employee_id=$2 AND (CASE WHEN p.kind='REVERSAL' THEN original.calculation_evidence ELSE p.calculation_evidence END)->>'periodStart'=$3::date::text AND (p.source_kind='PAYROLL' OR (p.kind='REVERSAL' AND original.source_kind='PAYROLL'))",vec![actor.tenant_id.into(),input.employee_id.into(),input.period_start.into()]).await?.ok_or(LoanModuleError::NotFound)?;
        budget = budget
            .checked_sub(store::decimal(&row, "recovered")?)
            .ok_or(LoanModuleError::InvalidCommand)?
            .max(Decimal::ZERO);
    }
    budget = budget.min(input.available_net);
    candidates.sort_by(|a, b| {
        let order = match priority {
            Some(RecoveryPriority::OldestApprovedFirst) => a.created_at.cmp(&b.created_at),
            Some(RecoveryPriority::NewestApprovedFirst) => b.created_at.cmp(&a.created_at),
            Some(RecoveryPriority::LowestBalanceFirst) => {
                a.line.principal_before.cmp(&b.line.principal_before)
            }
            Some(RecoveryPriority::HighestBalanceFirst) => {
                b.line.principal_before.cmp(&a.line.principal_before)
            }
            None => std::cmp::Ordering::Equal,
        };
        order.then_with(|| a.line.loan_id.cmp(&b.line.loan_id))
    });
    let requested = candidates.iter().try_fold(Decimal::ZERO, |total, value| {
        total
            .checked_add(value.line.requested)
            .ok_or(LoanModuleError::InvalidCommand)
    })?;
    if input.source == RecoverySource::Payroll
        && requested > budget
        && candidates.iter().any(|value| {
            value.line.requested > Decimal::ZERO
                && matches!(value.policy.short_salary, ShortSalaryPolicy::Block)
        })
    {
        return Err(LoanModuleError::RecoveryCapacity);
    }
    let mut lines = vec![];
    let mut total = Decimal::ZERO;
    for candidate in candidates {
        let mut line = candidate.line;
        let amount = line.requested.min(budget);
        let interest = line
            .interest_before
            .checked_add(line.accrued_interest)
            .ok_or(LoanModuleError::InvalidCommand)?;
        let allocation = allocate_repayment(&AllocationInput {
            payment: amount,
            principal: line.principal_before,
            interest,
            order: candidate.terms.allocation,
            currency: input.currency.clone(),
        })?;
        line.principal = allocation.principal;
        line.interest = allocation.interest;
        line.deferred = line
            .requested
            .checked_sub(amount)
            .ok_or(LoanModuleError::InvalidCommand)?;
        budget = budget
            .checked_sub(amount)
            .ok_or(LoanModuleError::InvalidCommand)?;
        total = total
            .checked_add(amount)
            .ok_or(LoanModuleError::InvalidCommand)?;
        lines.push(line);
    }
    let fingerprint = crate::command_hash(&(input, revision, &lines, total))?;
    Ok(RecoveryQuote {
        input: input.clone(),
        employee_revision: revision,
        lines,
        total,
        fingerprint,
    })
}
pub async fn validate_recovery_quote(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    quote: &RecoveryQuote,
) -> LoanResult<()> {
    let fresh = prepare_recovery_quote(tx, actor, &quote.input).await?;
    if fresh.fingerprint != quote.fingerprint {
        return Err(LoanModuleError::VersionConflict);
    }
    Ok(())
}
async fn post(
    tx: &DatabaseTransaction,
    input: &PostRecoveriesInput<'_>,
    source: RecoverySource,
) -> LoanResult<PostingResult> {
    let actor = input.actor;
    let quote = input.quote;
    if quote.input.source != source
        || quote.input.value_date > lifecycle::today(tx, actor.tenant_id).await?
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    validate_recovery_quote(tx, actor, quote).await?;
    let kind = match source {
        RecoverySource::Payroll => "PAYROLL",
        RecoverySource::Fnf => "FNF",
    };
    let mut ids = vec![];
    for line in &quote.lines {
        let amount = line
            .principal
            .checked_add(line.interest)
            .ok_or(LoanModuleError::InvalidCommand)?;
        if amount == Decimal::ZERO {
            continue;
        }
        let account = store::account(tx, actor.tenant_id, line.loan_id).await?;
        let version = store::terms(tx, &account).await?;
        let terms: LoanTerms = serde_json::from_value(version.terms)?;
        ids.extend(
            accrual::accrue(
                tx,
                actor,
                line.loan_id,
                version.id,
                &terms,
                quote.input.value_date,
                quote.input.source_id,
                quote.input.source_revision,
            )
            .await?,
        );
        ids.push(posting::post(tx,actor,Posting{loan:line.loan_id,terms:line.terms_version_id,kind:"REPAYMENT",source_kind:kind,source:quote.input.source_id,revision:quote.input.source_revision,date:quote.input.value_date,amount,principal:-line.principal,interest:-line.interest,reversal:None,evidence:serde_json::json!({"quoteFingerprint":quote.fingerprint,"employeeRevision":quote.employee_revision,"calculatorVersion":terms.calculator_version,"periodStart":quote.input.period_start})}).await?);
        posting::bump_account(tx, actor.tenant_id, line.loan_id).await?;
        store::execute(tx,"INSERT INTO loan_audit_event(id,tenant_id,loan_id,employee_id,actor_id,action,before_version,after_version,reason,\"references\") VALUES($1,$2,$3,$4,$5,$6,$7,$8,NULL,$9)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),line.loan_id.into(),quote.input.employee_id.into(),actor.user_id.into(),format!("{kind}_RECOVERY").into(),line.account_version.into(),line.account_version.checked_add(1).ok_or(LoanModuleError::InvalidCommand)?.into(),serde_json::json!({"sourceId":quote.input.source_id,"quoteFingerprint":quote.fingerprint}).into()]).await?;
    }
    let revision = if ids.is_empty() {
        quote.employee_revision
    } else {
        store::advance_revision(tx, actor.tenant_id, quote.input.employee_id, kind).await?
    };
    Ok(PostingResult {
        posting_ids: ids,
        employee_revision: revision,
        total: quote.total,
    })
}
pub async fn post_payroll_recoveries(
    tx: &DatabaseTransaction,
    input: &PostRecoveriesInput<'_>,
) -> LoanResult<PostingResult> {
    post(tx, input, RecoverySource::Payroll).await
}
pub async fn post_fnf_recoveries(
    tx: &DatabaseTransaction,
    input: &PostRecoveriesInput<'_>,
) -> LoanResult<PostingResult> {
    post(tx, input, RecoverySource::Fnf).await
}
