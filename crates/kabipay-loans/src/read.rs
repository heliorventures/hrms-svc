use crate::{
    access, repository as store, LoanActorScope, LoanModuleError, LoanPermission, LoanResult,
};
use chrono::NaiveDate;
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use sea_orm::DatabaseTransaction;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanRequestView {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub requested_amount: String,
    pub currency: String,
    pub purpose: String,
    pub employee_notes: Option<String>,
    pub state: String,
    pub version: i64,
    pub workflow_instance_id: Option<Uuid>,
    pub current_step_id: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanAccountView {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub request_id: Uuid,
    pub loan_number: String,
    pub approved_principal: String,
    pub currency: String,
    pub minor_units: i32,
    pub state: String,
    pub funding_state: String,
    pub version: i64,
    pub principal: String,
    pub interest: String,
    pub terms: serde_json::Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanPolicyView {
    pub id: Uuid,
    pub key: String,
    pub version: i32,
    pub status: String,
    pub currency: String,
    pub minor_units: i32,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub rules: serde_json::Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanPostingView {
    pub id: Uuid,
    pub kind: String,
    pub source_kind: String,
    pub source_id: Uuid,
    pub value_date: NaiveDate,
    pub amount: String,
    pub principal_delta: String,
    pub interest_delta: String,
    pub reversal_of: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanPaymentView {
    pub id: Uuid,
    pub kind: String,
    pub amount: String,
    pub value_date: NaiveDate,
    pub method: String,
    pub external_reference: String,
    pub unapplied_credit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanScheduleView {
    pub id: Uuid,
    pub version: i64,
    pub effective_from: NaiveDate,
    pub monthly_amount: String,
    pub recovery_mode: String,
    pub is_projection: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanPage<T> {
    pub nodes: Vec<T>,
    pub end_cursor: Option<Uuid>,
    pub has_next_page: bool,
}
pub(crate) fn limit(first: u32) -> LoanResult<usize> {
    if first == 0 || first > 100 {
        return Err(LoanModuleError::InvalidCommand);
    }
    Ok(first as usize)
}
fn page<T>(mut nodes: Vec<T>, limit: usize, id: impl Fn(&T) -> Uuid) -> LoanPage<T> {
    let next = nodes.len() > limit;
    nodes.truncate(limit);
    let cursor = nodes.last().map(id);
    LoanPage {
        nodes,
        end_cursor: cursor,
        has_next_page: next,
    }
}
async fn readable(tx: &DatabaseTransaction, actor: &LoanActorScope) -> LoanResult<()> {
    if !matches!(
        actor.permission,
        LoanPermission::Read | LoanPermission::Export
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await
}
async fn scope_ids(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    self_only: bool,
    target: Option<Uuid>,
) -> LoanResult<Option<Vec<Uuid>>> {
    if self_only {
        let employee = access::viewer(tx, actor)
            .await?
            .ok_or(LoanModuleError::Forbidden)?
            .employee_id;
        access::employee(tx, actor, employee).await?;
        return Ok(Some(vec![employee]));
    }
    if let Some(id) = target {
        access::employee(tx, actor, id).await?;
        return Ok(Some(vec![id]));
    }
    Ok(match access::filter(tx, actor).await? {
        EmployeeScopeFilter::Unrestricted => None,
        EmployeeScopeFilter::Empty => Some(vec![]),
        EmployeeScopeFilter::EmployeeIds(ids) => Some(ids),
    })
}
/// Filtering happens in SQL before pagination, not after exposing/reading other employees' rows.
pub async fn list_accounts(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    self_only: bool,
    employee: Option<Uuid>,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanAccountView>> {
    readable(tx, actor).await?;
    let count = limit(first)?;
    let ids = scope_ids(tx, actor, self_only, employee).await?;
    let rows=store::all(tx,"SELECT a.id,a.employee_id,a.request_id,a.loan_number,a.approved_principal::text AS approved_principal,a.currency,a.minor_units,a.state,a.funding_state,a.version,t.terms,COALESCE((SELECT SUM(principal_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS principal,COALESCE((SELECT SUM(interest_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS interest FROM loan_account a JOIN loan_terms_version t ON t.tenant_id=a.tenant_id AND t.loan_id=a.id AND t.id=a.current_terms_id WHERE a.tenant_id=$1 AND ($2::uuid[] IS NULL OR a.employee_id=ANY($2)) AND ($3::uuid IS NULL OR a.id>$3) ORDER BY a.id LIMIT $4",vec![actor.tenant_id.into(),ids.into(),after.into(),(i64::from(first)+1).into()]).await?;
    let nodes = rows
        .into_iter()
        .map(|row| {
            Ok(LoanAccountView {
                id: row.try_get("", "id")?,
                employee_id: row.try_get("", "employee_id")?,
                request_id: row.try_get("", "request_id")?,
                loan_number: row.try_get("", "loan_number")?,
                approved_principal: row.try_get("", "approved_principal")?,
                currency: row.try_get("", "currency")?,
                minor_units: row.try_get("", "minor_units")?,
                state: row.try_get("", "state")?,
                funding_state: row.try_get("", "funding_state")?,
                version: row.try_get("", "version")?,
                principal: row.try_get("", "principal")?,
                interest: row.try_get("", "interest")?,
                terms: row.try_get("", "terms")?,
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
pub async fn list_requests(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    self_only: bool,
    employee: Option<Uuid>,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanRequestView>> {
    readable(tx, actor).await?;
    let count = limit(first)?;
    let ids = scope_ids(tx, actor, self_only, employee).await?;
    let rows=store::all(tx,"SELECT r.id,r.employee_id,r.requested_amount::text AS requested_amount,r.currency,r.purpose,r.employee_notes,r.state,r.version,r.workflow_instance_id,w.current_step_id FROM loan_request r LEFT JOIN workflow_instance w ON w.tenant_id=r.tenant_id AND w.id=r.workflow_instance_id WHERE r.tenant_id=$1 AND ($2::uuid[] IS NULL OR r.employee_id=ANY($2)) AND ($3::uuid IS NULL OR r.id>$3) AND ($5::bool OR r.state<>'DRAFT') ORDER BY r.id LIMIT $4",vec![actor.tenant_id.into(),ids.into(),after.into(),(i64::from(first)+1).into(),self_only.into()]).await?;
    let nodes = rows
        .into_iter()
        .map(|row| {
            Ok(LoanRequestView {
                id: row.try_get("", "id")?,
                employee_id: row.try_get("", "employee_id")?,
                requested_amount: row.try_get("", "requested_amount")?,
                currency: row.try_get("", "currency")?,
                purpose: row.try_get("", "purpose")?,
                employee_notes: row.try_get("", "employee_notes")?,
                state: row.try_get("", "state")?,
                version: row.try_get("", "version")?,
                workflow_instance_id: row.try_get("", "workflow_instance_id")?,
                current_step_id: row.try_get("", "current_step_id")?,
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
pub async fn loan_account(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    id: Uuid,
) -> LoanResult<LoanAccountView> {
    readable(tx, actor).await?;
    // Detail uses the same scoped query contract; SQL's exact ID prevents cursor substitution.
    let account = store::account(tx, actor.tenant_id, id).await?;
    access::employee(tx, actor, account.employee_id).await?;
    let version = store::terms(tx, &account).await?;
    let (principal, interest) = store::balances(tx, actor.tenant_id, id).await?;
    Ok(LoanAccountView {
        id,
        employee_id: account.employee_id,
        request_id: account.request_id,
        loan_number: account.loan_number,
        approved_principal: account.approved_principal.to_string(),
        currency: account.currency,
        minor_units: account.minor_units,
        state: account.state,
        funding_state: account.funding_state,
        version: account.version,
        principal: principal.to_string(),
        interest: interest.to_string(),
        terms: version.terms,
    })
}
pub async fn list_policies(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanPolicyView>> {
    readable(tx, actor).await?;
    policy_page(tx, actor, after, first, false).await
}
pub async fn loan_policy_versions(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanPolicyView>> {
    if actor.permission != LoanPermission::Policy {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    access::company(actor)?;
    policy_page(tx, actor, after, first, true).await
}
async fn policy_page(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    after: Option<Uuid>,
    first: u32,
    history: bool,
) -> LoanResult<LoanPage<LoanPolicyView>> {
    let count = limit(first)?;
    let nodes=store::all(tx,"SELECT id,policy_key,version,status,currency,minor_units,effective_from,effective_to,rules FROM loan_policy_version WHERE tenant_id=$1 AND ($4::bool OR status='ACTIVE') AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT $3",vec![actor.tenant_id.into(),after.into(),(i64::from(first)+1).into(),history.into()]).await?.into_iter().map(|row|Ok(LoanPolicyView{id:row.try_get("","id")?,key:row.try_get("","policy_key")?,version:row.try_get("","version")?,status:row.try_get("","status")?,currency:row.try_get("","currency")?,minor_units:row.try_get("","minor_units")?,effective_from:row.try_get("","effective_from")?,effective_to:row.try_get("","effective_to")?,rules:row.try_get("","rules")?})).collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
pub async fn loan_ledger(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanPostingView>> {
    readable(tx, actor).await?;
    let count = limit(first)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await?;
    if let Some(after) = after {
        if store::one(
            tx,
            "SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND id=$3",
            vec![actor.tenant_id.into(), loan.into(), after.into()],
        )
        .await?
        .is_none()
        {
            return Err(LoanModuleError::InvalidCommand);
        }
    }
    let nodes=store::all(tx,"SELECT p.id,p.kind,p.source_kind,p.source_id,p.value_date,p.amount::text AS amount,p.principal_delta::text AS principal_delta,p.interest_delta::text AS interest_delta,p.reversal_of FROM loan_posting p JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id WHERE p.tenant_id=$1 AND p.loan_id=$2 AND ($3::uuid IS NULL OR l.sequence>(SELECT sequence FROM loan_ledger_entry WHERE tenant_id=$1 AND loan_id=$2 AND posting_id=$3)) ORDER BY l.sequence LIMIT $4",vec![actor.tenant_id.into(),loan.into(),after.into(),(i64::from(first)+1).into()]).await?.into_iter().map(|row|Ok(LoanPostingView{id:row.try_get("","id")?,kind:row.try_get("","kind")?,source_kind:row.try_get("","source_kind")?,source_id:row.try_get("","source_id")?,value_date:row.try_get("","value_date")?,amount:row.try_get("","amount")?,principal_delta:row.try_get("","principal_delta")?,interest_delta:row.try_get("","interest_delta")?,reversal_of:row.try_get("","reversal_of")?})).collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
pub async fn loan_payments(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanPaymentView>> {
    readable(tx, actor).await?;
    let count = limit(first)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await?;
    let nodes=store::all(tx,"WITH payments AS (SELECT id,'DISBURSEMENT'::text AS kind,amount::text AS amount,value_date,method,external_reference,'0'::text AS unapplied_credit FROM loan_disbursement WHERE tenant_id=$1 AND loan_id=$2 UNION ALL SELECT r.id,'RECEIPT',r.amount::text,r.value_date,r.method,r.external_reference,COALESCE((SELECT SUM(remaining_amount) FROM loan_unapplied_credit c WHERE c.tenant_id=r.tenant_id AND c.loan_id=r.loan_id AND c.receipt_id=r.id),0)::text FROM loan_receipt r WHERE r.tenant_id=$1 AND r.loan_id=$2) SELECT * FROM payments WHERE ($3::uuid IS NULL OR id>$3) ORDER BY id LIMIT $4",vec![actor.tenant_id.into(),loan.into(),after.into(),(i64::from(first)+1).into()]).await?.into_iter().map(|row|Ok(LoanPaymentView{id:row.try_get("","id")?,kind:row.try_get("","kind")?,amount:row.try_get("","amount")?,value_date:row.try_get("","value_date")?,method:row.try_get("","method")?,external_reference:row.try_get("","external_reference")?,unapplied_credit:row.try_get("","unapplied_credit")?})).collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
pub async fn loan_schedules(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanScheduleView>> {
    readable(tx, actor).await?;
    let count = limit(first)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await?;
    let nodes=store::all(tx,"SELECT id,version,effective_from,monthly_amount::text AS monthly_amount,recovery_mode FROM loan_schedule_version WHERE tenant_id=$1 AND loan_id=$2 AND ($3::uuid IS NULL OR id>$3) ORDER BY id LIMIT $4",vec![actor.tenant_id.into(),loan.into(),after.into(),(i64::from(first)+1).into()]).await?.into_iter().map(|row|Ok(LoanScheduleView{id:row.try_get("","id")?,version:row.try_get("","version")?,effective_from:row.try_get("","effective_from")?,monthly_amount:row.try_get("","monthly_amount")?,recovery_mode:row.try_get("","recovery_mode")?,is_projection:true})).collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
}
