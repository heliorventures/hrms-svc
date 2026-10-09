use crate::{
    access, repository as store, workflow, LoanActorScope, LoanCommandResult, LoanModuleError,
    LoanResult, RequestDecision,
};
use chrono::NaiveDate;
use kabipay_loans_domain::{Currency, LoanPolicy, LoanTerms, CALCULATOR_VERSION};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) fn result(
    id: Uuid,
    loan: Option<Uuid>,
    employee: Option<Uuid>,
    version: i64,
    state: &str,
) -> LoanCommandResult {
    LoanCommandResult {
        record_id: id,
        loan_id: loan,
        employee_id: employee,
        version,
        state: state.into(),
        posting_ids: vec![],
        unapplied_credit: rust_decimal::Decimal::ZERO,
    }
}
pub(crate) fn checked_version(actual: i64, expected: i64) -> LoanResult<()> {
    if actual != expected {
        return Err(LoanModuleError::VersionConflict);
    }
    Ok(())
}
pub(crate) fn text(value: &str, max: usize) -> LoanResult<()> {
    if value.trim().is_empty() || value.len() > max {
        return Err(LoanModuleError::InvalidCommand);
    }
    Ok(())
}
pub(crate) async fn today(tx: &DatabaseTransaction, tenant: Uuid) -> LoanResult<NaiveDate> {
    let row = store::one(
        tx,
        "SELECT timezone FROM kabipay_ops.tenant WHERE id=$1 AND is_deleted=FALSE",
        vec![tenant.into()],
    )
    .await?
    .ok_or(LoanModuleError::NotFound)?;
    let timezone: Option<String> = row.try_get("", "timezone")?;
    Ok(
        kabipay_common::tenant_business_clock::TenantBusinessClock::from_configured_name(
            timezone.as_deref(),
        )?
        .now_date(),
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    key: &str,
    currency: &Currency,
    from: NaiveDate,
    to: Option<NaiveDate>,
    rules: &LoanPolicy,
) -> LoanResult<LoanCommandResult> {
    access::company(actor)?;
    text(key, 100)?;
    currency.validate()?;
    rules.validate(currency)?;
    if to.is_some_and(|to| to <= from) {
        return Err(LoanModuleError::InvalidCommand);
    }
    let row = store::one(
        tx,
        "SELECT currency FROM kabipay_ops.tenant WHERE id=$1",
        vec![actor.tenant_id.into()],
    )
    .await?
    .ok_or(LoanModuleError::NotFound)?;
    let company_currency: Option<String> = row.try_get("", "currency")?;
    if company_currency.as_deref() != Some(currency.code.as_str()) {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    let workflow_id = rules
        .approval
        .workflow_id
        .parse()
        .map_err(|_| LoanModuleError::PolicyUnavailable)?;
    workflow::configured(tx, actor.tenant_id, workflow_id).await?;
    let row=store::one(tx,"SELECT COALESCE(MAX(version),0) AS version FROM loan_policy_version WHERE tenant_id=$1 AND policy_key=$2",vec![actor.tenant_id.into(),key.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let previous: i32 = row.try_get("", "version")?;
    checked_version(i64::from(previous.max(1)), expected)?;
    let version = previous
        .checked_add(1)
        .ok_or(LoanModuleError::InvalidCommand)?;
    let id = Uuid::new_v4();
    store::execute(tx,"INSERT INTO loan_policy_version(id,tenant_id,policy_key,version,status,currency,minor_units,effective_from,effective_to,rules,calculator_version,approved_by) VALUES($1,$2,$3,$4,'ACTIVE',$5,$6,$7,$8,$9,$10,$11)",vec![id.into(),actor.tenant_id.into(),key.into(),version.into(),currency.code.clone().into(),(currency.minor_units as i32).into(),from.into(),to.into(),serde_json::to_value(rules)?.into(),CALCULATOR_VERSION.into(),actor.user_id.into()]).await?;
    Ok(result(id, None, None, i64::from(version), "ACTIVE"))
}
pub(crate) async fn selected_policy(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
    date: NaiveDate,
) -> LoanResult<(Currency, LoanPolicy)> {
    let policy = store::policy(tx, tenant, id).await?;
    if policy.status != "ACTIVE"
        || policy.effective_from > date
        || policy.effective_to.is_some_and(|to| date >= to)
        || policy.calculator_version != CALCULATOR_VERSION
    {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    let currency = Currency {
        code: policy.currency,
        minor_units: u32::try_from(policy.minor_units)
            .map_err(|_| LoanModuleError::PolicyUnavailable)?,
    };
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate(&currency)?;
    Ok((currency, rules))
}
pub(crate) async fn retire_policy(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    id: Uuid,
    reason: &str,
) -> LoanResult<LoanCommandResult> {
    access::company(actor)?;
    text(reason, 2000)?;
    let policy = store::policy(tx, actor.tenant_id, id).await?;
    checked_version(i64::from(policy.version), expected)?;
    if policy.status != "ACTIVE" {
        return Err(LoanModuleError::VersionConflict);
    }
    store::execute(
        tx,
        "UPDATE loan_policy_version SET status='RETIRED' WHERE tenant_id=$1 AND id=$2",
        vec![actor.tenant_id.into(), id.into()],
    )
    .await?;
    Ok(result(id, None, None, i64::from(policy.version), "RETIRED"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestStage {
    Draft,
    Submitted,
}
async fn eligible(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
    date: NaiveDate,
    rules: &LoanPolicy,
) -> LoanResult<()> {
    let row=store::one(tx,"SELECT employment_type,date_of_joining,status FROM employee WHERE tenant_id=$1 AND id=$2 AND is_deleted=FALSE FOR UPDATE",vec![tenant.into(),employee.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let employment_type: Option<String> = row.try_get("", "employment_type")?;
    let joined: NaiveDate = row.try_get("", "date_of_joining")?;
    let status: String = row.try_get("", "status")?;
    if !kabipay_common::context::is_active_employment_status(&status)
        || employment_type
            .as_ref()
            .is_none_or(|kind| !rules.eligibility.employment_types.contains(kind))
        || (date - joined).num_days() < i64::from(rules.eligibility.minimum_service_days)
    {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    request_id: Option<Uuid>,
    employee_id: Option<Uuid>,
    policy_id: Uuid,
    amount: rust_decimal::Decimal,
    purpose: &str,
    notes: &Option<String>,
    preferences: &serde_json::Value,
    stage: RequestStage,
) -> LoanResult<LoanCommandResult> {
    text(purpose, 255)?;
    if notes.as_ref().is_some_and(|v| v.len() > 4000)
        || serde_json::to_vec(preferences)?.len() > 8192
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    let employee = match employee_id {
        Some(id) => id,
        None => {
            access::viewer(tx, actor)
                .await?
                .ok_or(LoanModuleError::Forbidden)?
                .employee_id
        }
    };
    access::employee(tx, actor, employee).await?;
    let date = today(tx, actor.tenant_id).await?;
    let (currency, rules) = selected_policy(tx, actor.tenant_id, policy_id, date).await?;
    eligible(tx, actor.tenant_id, employee, date, &rules).await?;
    currency.validate_amount(amount)?;
    if amount <= rust_decimal::Decimal::ZERO || amount > rules.exposure.per_loan {
        return Err(LoanModuleError::InvalidCommand);
    }
    let prefs = serde_json::json!({"policyId":policy_id,"employee":preferences});
    let state = match stage {
        RequestStage::Draft => "DRAFT",
        RequestStage::Submitted => "SUBMITTED",
    };
    let (id, version) = if let Some(id) = request_id {
        let record = store::request(tx, actor.tenant_id, id).await?;
        checked_version(record.version, expected)?;
        if record.employee_id != employee || !matches!(record.state.as_str(), "DRAFT" | "RETURNED")
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        store::execute(tx,"UPDATE loan_request SET requested_amount=$3,currency=$4,purpose=$5,employee_notes=$6,preferences=$7,state=$8,workflow_instance_id=NULL,version=version+1 WHERE tenant_id=$1 AND id=$2",vec![actor.tenant_id.into(),id.into(),amount.into(),currency.code.into(),purpose.into(),notes.clone().into(),prefs.into(),state.into()]).await?;
        (
            id,
            record
                .version
                .checked_add(1)
                .ok_or(LoanModuleError::InvalidCommand)?,
        )
    } else {
        checked_version(1, expected)?;
        let id = Uuid::new_v4();
        store::execute(tx,"INSERT INTO loan_request(id,tenant_id,employee_id,requested_amount,currency,purpose,employee_notes,preferences,state,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1)",vec![id.into(),actor.tenant_id.into(),employee.into(),amount.into(),currency.code.into(),purpose.into(),notes.clone().into(),prefs.into(),state.into()]).await?;
        (id, 1)
    };
    if stage == RequestStage::Draft {
        return Ok(result(id, None, Some(employee), version, state));
    }
    let instance = workflow::attach(
        tx,
        actor,
        id,
        rules
            .approval
            .workflow_id
            .parse()
            .map_err(|_| LoanModuleError::PolicyUnavailable)?,
    )
    .await?;
    store::execute(
        tx,
        "UPDATE loan_request SET workflow_instance_id=$3 WHERE tenant_id=$1 AND id=$2",
        vec![actor.tenant_id.into(), id.into(), instance.into()],
    )
    .await?;
    Ok(result(id, None, Some(employee), version, "SUBMITTED"))
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn decide(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    id: Uuid,
    step: Uuid,
    decision: RequestDecision,
    reason: &str,
    terms: Option<&LoanTerms>,
    from: Option<NaiveDate>,
    first_due: Option<NaiveDate>,
    agreement: Option<&str>,
) -> LoanResult<LoanCommandResult> {
    text(reason, 2000)?;
    let record = store::request(tx, actor.tenant_id, id).await?;
    checked_version(record.version, expected)?;
    access::employee(tx, actor, record.employee_id).await?;
    if !matches!(record.state.as_str(), "SUBMITTED" | "UNDER_REVIEW") {
        return Err(LoanModuleError::InvalidCommand);
    }
    let final_step = workflow::decide(
        tx,
        actor,
        record.employee_id,
        id,
        record
            .workflow_instance_id
            .ok_or(LoanModuleError::PolicyUnavailable)?,
        step,
        decision,
        reason,
    )
    .await?;
    if !final_step
        && (terms.is_some() || from.is_some() || first_due.is_some() || agreement.is_some())
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    let state = match decision {
        RequestDecision::Approve if final_step => "APPROVED",
        RequestDecision::Approve => "UNDER_REVIEW",
        RequestDecision::Return => "RETURNED",
        RequestDecision::Reject => "REJECTED",
    };
    let loan = if final_step {
        let terms = terms.ok_or(LoanModuleError::InvalidCommand)?;
        let from = from.ok_or(LoanModuleError::InvalidCommand)?;
        let first = first_due.ok_or(LoanModuleError::InvalidCommand)?;
        if from < today(tx, actor.tenant_id).await?
            || first < from
            || terms.approved_principal > record.requested_amount
            || terms.currency.code != record.currency
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        let policy_id: Uuid = serde_json::from_value(
            record
                .preferences
                .get("policyId")
                .cloned()
                .ok_or(LoanModuleError::PolicyUnavailable)?,
        )?;
        let (currency, rules) = selected_policy(tx, actor.tenant_id, policy_id, from).await?;
        if currency != terms.currency {
            return Err(LoanModuleError::InvalidCommand);
        }
        rules.validate_terms(terms)?;
        eligible(tx, actor.tenant_id, record.employee_id, from, &rules).await?;
        if rules.approval.acknowledgement_required {
            text(agreement.ok_or(LoanModuleError::InvalidCommand)?, 500)?;
        }
        exposure(tx, actor.tenant_id, record.employee_id, terms, &rules).await?;
        let loan = Uuid::new_v4();
        let terms_id = Uuid::new_v4();
        store::execute(tx,"INSERT INTO loan_account(id,tenant_id,employee_id,request_id,loan_number,approved_principal,currency,minor_units,state,funding_state,current_terms_id,rounding_carry,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'OPEN','UNFUNDED',$9,0,1)",vec![loan.into(),actor.tenant_id.into(),record.employee_id.into(),id.into(),format!("LN-{loan}").into(),terms.approved_principal.into(),terms.currency.code.clone().into(),(terms.currency.minor_units as i32).into(),terms_id.into()]).await?;
        store::execute(tx,"INSERT INTO loan_terms_version(id,tenant_id,loan_id,policy_version_id,version,effective_from,terms,approved_by,agreement_evidence) VALUES($1,$2,$3,$4,1,$5,$6,$7,$8)",vec![terms_id.into(),actor.tenant_id.into(),loan.into(),policy_id.into(),from.into(),serde_json::to_value(terms)?.into(),actor.user_id.into(),serde_json::json!({"reference":agreement,"recordedBy":actor.user_id}).into()]).await?;
        crate::arrangement::initial(tx, actor, loan, terms_id, terms, from, first, reason).await?;
        Some(loan)
    } else {
        None
    };
    store::execute(tx,"UPDATE loan_request SET state=$3,management_notes=$4,version=version+1 WHERE tenant_id=$1 AND id=$2",vec![actor.tenant_id.into(),id.into(),state.into(),reason.into()]).await?;
    Ok(result(
        id,
        loan,
        Some(record.employee_id),
        record
            .version
            .checked_add(1)
            .ok_or(LoanModuleError::InvalidCommand)?,
        state,
    ))
}
async fn exposure(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    employee: Uuid,
    terms: &LoanTerms,
    rules: &LoanPolicy,
) -> LoanResult<()> {
    // Outstanding principal plus undisbursed commitments. Paid debt releases exposure.
    let row=store::one(tx,"WITH exposure AS (SELECT a.employee_id,COALESCE((SELECT SUM(principal_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)+GREATEST(a.approved_principal-COALESCE((SELECT SUM(CASE WHEN kind='DISBURSEMENT' THEN principal_delta WHEN kind='REVERSAL' AND principal_delta<0 THEN principal_delta ELSE 0 END) FROM loan_posting p WHERE p.tenant_id=a.tenant_id AND p.loan_id=a.id),0),0) AS amount FROM loan_account a WHERE a.tenant_id=$1 AND a.state='OPEN' AND a.currency=$3) SELECT COALESCE(SUM(amount) FILTER(WHERE employee_id=$2),0)::text AS employee,COALESCE(SUM(amount),0)::text AS company FROM exposure",vec![tenant.into(),employee.into(),terms.currency.code.clone().into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let add = |value: rust_decimal::Decimal| {
        value
            .checked_add(terms.approved_principal)
            .ok_or(LoanModuleError::InvalidCommand)
    };
    if add(store::decimal(&row, "employee")?)? > rules.exposure.per_employee
        || add(store::decimal(&row, "company")?)? > rules.exposure.per_company
    {
        return Err(LoanModuleError::ExposureExceeded);
    }
    Ok(())
}
pub(crate) async fn withdraw(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    id: Uuid,
    reason: &str,
) -> LoanResult<LoanCommandResult> {
    text(reason, 2000)?;
    let record = store::request(tx, actor.tenant_id, id).await?;
    checked_version(record.version, expected)?;
    access::employee(tx, actor, record.employee_id).await?;
    if !matches!(
        record.state.as_str(),
        "DRAFT" | "SUBMITTED" | "UNDER_REVIEW" | "RETURNED"
    ) {
        return Err(LoanModuleError::InvalidCommand);
    }
    if let Some(instance) = record.workflow_instance_id {
        workflow::withdraw(tx, actor.tenant_id, instance).await?;
    }
    store::execute(
        tx,
        "UPDATE loan_request SET state='WITHDRAWN',version=version+1 WHERE tenant_id=$1 AND id=$2",
        vec![actor.tenant_id.into(), id.into()],
    )
    .await?;
    Ok(result(
        id,
        None,
        Some(record.employee_id),
        record
            .version
            .checked_add(1)
            .ok_or(LoanModuleError::InvalidCommand)?,
        "WITHDRAWN",
    ))
}
