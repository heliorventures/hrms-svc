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
    pub employee_name: String,
    pub employee_code: String,
    pub policy_id: Uuid,
    pub preferences: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub requested_amount: String,
    pub currency: String,
    pub purpose: String,
    pub employee_notes: Option<String>,
    pub state: String,
    pub version: i64,
    pub workflow_instance_id: Option<Uuid>,
    pub current_step_id: Option<Uuid>,
    pub is_final_approval_step: bool,
    pub decision_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanAccountView {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub request_id: Uuid,
    pub loan_number: String,
    pub employee_name: String,
    pub employee_code: String,
    pub purpose: String,
    pub disbursed: String,
    pub repaid: String,
    pub outstanding: String,
    pub unapplied_credit: String,
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
    pub resignation_exception_reason: Option<String>,
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
#[derive(Clone, Debug)]
pub struct LoanApprovalWorkflowView {
    pub id: Uuid,
    pub name: String,
}
pub async fn loan_approval_workflows(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    after: Option<Uuid>,
    first: u32,
) -> LoanResult<LoanPage<LoanApprovalWorkflowView>> {
    if actor.permission != LoanPermission::Policy
        || actor.scope != kabipay_common::context::ScopeType::All
    {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    let count = limit(first)?;
    let rows=store::all(tx,"SELECT w.id,w.name FROM workflow w WHERE w.tenant_id=$1 AND w.is_active AND w.entity_type='LOAN_REQUEST' AND ($2::uuid IS NULL OR w.id>$2) AND EXISTS(SELECT 1 FROM workflow_step s WHERE s.tenant_id=w.tenant_id AND s.workflow_id=w.id) AND NOT EXISTS(SELECT 1 FROM workflow_step s WHERE s.tenant_id=w.tenant_id AND s.workflow_id=w.id AND s.approver_permission IS DISTINCT FROM 'loan:approve') ORDER BY w.id LIMIT $3",vec![actor.tenant_id.into(),after.into(),(i64::from(first)+1).into()]).await?;
    let nodes = rows
        .into_iter()
        .map(|r| {
            Ok(LoanApprovalWorkflowView {
                id: r.try_get("", "id")?,
                name: r.try_get("", "name")?,
            })
        })
        .collect::<LoanResult<Vec<_>>>()?;
    Ok(page(nodes, count, |v| v.id))
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
#[derive(Default)]
pub struct LoanReadFilter {
    pub search: Option<String>,
    pub state: Option<String>,
}
impl LoanReadFilter {
    fn validate(&self) -> LoanResult<()> {
        if self.search.as_ref().is_some_and(|v| v.len() > 200)
            || self.state.as_ref().is_some_and(|v| v.len() > 32)
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        Ok(())
    }
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
    list_filtered_accounts(
        tx,
        actor,
        self_only,
        employee,
        after,
        first,
        &LoanReadFilter::default(),
    )
    .await
}
pub async fn list_filtered_accounts(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    self_only: bool,
    employee: Option<Uuid>,
    after: Option<Uuid>,
    first: u32,
    filter: &LoanReadFilter,
) -> LoanResult<LoanPage<LoanAccountView>> {
    filter.validate()?;
    readable(tx, actor).await?;
    let count = limit(first)?;
    let ids = scope_ids(tx, actor, self_only, employee).await?;
    let rows=store::all(tx,"SELECT e.first_name || ' ' || e.last_name AS employee_name,e.employee_code,r.purpose,COALESCE((SELECT SUM(p.amount) FROM loan_posting p WHERE p.tenant_id=a.tenant_id AND p.loan_id=a.id AND p.kind='DISBURSEMENT' AND NOT EXISTS(SELECT 1 FROM loan_posting x WHERE x.tenant_id=p.tenant_id AND x.reversal_of=p.id)),0)::text AS disbursed,COALESCE((SELECT SUM(p.amount) FROM loan_posting p WHERE p.tenant_id=a.tenant_id AND p.loan_id=a.id AND p.kind='REPAYMENT' AND NOT EXISTS(SELECT 1 FROM loan_posting x WHERE x.tenant_id=p.tenant_id AND x.reversal_of=p.id)),0)::text AS repaid,COALESCE((SELECT SUM(principal_delta+interest_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS outstanding,COALESCE((SELECT SUM(remaining_amount) FROM loan_unapplied_credit c WHERE c.tenant_id=a.tenant_id AND c.loan_id=a.id),0)::text AS unapplied_credit,a.id,a.employee_id,a.request_id,a.loan_number,a.approved_principal::text AS approved_principal,a.currency,a.minor_units,a.state,a.funding_state,a.version,t.terms,COALESCE((SELECT SUM(principal_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS principal,COALESCE((SELECT SUM(interest_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS interest FROM loan_account a JOIN employee e ON e.tenant_id=a.tenant_id AND e.id=a.employee_id JOIN loan_request r ON r.tenant_id=a.tenant_id AND r.id=a.request_id JOIN loan_terms_version t ON t.tenant_id=a.tenant_id AND t.loan_id=a.id AND t.id=a.current_terms_id WHERE a.tenant_id=$1 AND ($2::uuid[] IS NULL OR a.employee_id=ANY($2)) AND ($3::uuid IS NULL OR a.id>$3) AND ($5::text IS NULL OR strpos(lower(concat_ws(' ',a.loan_number,e.first_name,e.last_name,e.employee_code,r.purpose)),lower($5))>0) AND ($6::text IS NULL OR a.state=$6) ORDER BY a.id LIMIT $4",vec![actor.tenant_id.into(),ids.into(),after.into(),(i64::from(first)+1).into(),filter.search.clone().into(),filter.state.clone().into()]).await?;
    let nodes = rows
        .into_iter()
        .map(|row| {
            Ok(LoanAccountView {
                id: row.try_get("", "id")?,
                employee_id: row.try_get("", "employee_id")?,
                request_id: row.try_get("", "request_id")?,
                loan_number: row.try_get("", "loan_number")?,
                employee_name: row.try_get("", "employee_name")?,
                employee_code: row.try_get("", "employee_code")?,
                purpose: row.try_get("", "purpose")?,
                disbursed: row.try_get("", "disbursed")?,
                repaid: row.try_get("", "repaid")?,
                outstanding: row.try_get("", "outstanding")?,
                unapplied_credit: row.try_get("", "unapplied_credit")?,
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
    list_filtered_requests(
        tx,
        actor,
        self_only,
        employee,
        after,
        first,
        &LoanReadFilter::default(),
    )
    .await
}
pub async fn list_filtered_requests(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    self_only: bool,
    employee: Option<Uuid>,
    after: Option<Uuid>,
    first: u32,
    filter: &LoanReadFilter,
) -> LoanResult<LoanPage<LoanRequestView>> {
    filter.validate()?;
    readable(tx, actor).await?;
    let count = limit(first)?;
    let ids = scope_ids(tx, actor, self_only, employee).await?;
    // Match workflow::decide's next-step rule in the same read snapshot as the
    // request version. This is presentation metadata; writes still check authority.
    let rows = store::all(
        tx,
        r#"SELECT e.first_name || ' ' || e.last_name AS employee_name,
            e.employee_code, (r.preferences->>'policyId')::uuid AS policy_id,
            r.preferences->'employee' AS preferences, r.created_at, r.id,
            r.employee_id, r.requested_amount::text AS requested_amount,
            r.currency, r.purpose, r.employee_notes, r.state, r.version,
            r.workflow_instance_id, w.current_step_id,
            r.management_notes AS decision_reason,
            EXISTS (
                SELECT 1 FROM workflow_step s
                WHERE s.tenant_id=r.tenant_id AND s.workflow_id=w.workflow_id
                  AND s.id=w.current_step_id AND w.status='IN_PROGRESS'
                  AND r.state IN ('SUBMITTED','UNDER_REVIEW')
                  AND NOT EXISTS (
                      SELECT 1 FROM workflow_step next_step
                      WHERE next_step.tenant_id=s.tenant_id
                        AND next_step.workflow_id=s.workflow_id
                        AND next_step.sequence_order>s.sequence_order
                  )
            ) AS is_final_approval_step
        FROM loan_request r
        JOIN employee e ON e.tenant_id=r.tenant_id AND e.id=r.employee_id
        LEFT JOIN workflow_instance w ON w.tenant_id=r.tenant_id
            AND w.id=r.workflow_instance_id AND w.entity_type='LOAN_REQUEST'
            AND w.entity_id=r.id
        WHERE r.tenant_id=$1 AND ($2::uuid[] IS NULL OR r.employee_id=ANY($2))
            AND ($3::uuid IS NULL OR r.id>$3) AND ($5::bool OR r.state<>'DRAFT')
            AND ($6::text IS NULL OR strpos(lower(concat_ws(' ',e.first_name,
                e.last_name,e.employee_code,r.purpose)),lower($6))>0)
            AND ($7::text IS NULL OR r.state=$7)
        ORDER BY r.id LIMIT $4"#,
        vec![
            actor.tenant_id.into(),
            ids.into(),
            after.into(),
            (i64::from(first) + 1).into(),
            self_only.into(),
            filter.search.clone().into(),
            filter.state.clone().into(),
        ],
    )
    .await?;
    let nodes = rows
        .into_iter()
        .map(|row| {
            Ok(LoanRequestView {
                id: row.try_get("", "id")?,
                employee_id: row.try_get("", "employee_id")?,
                employee_name: row.try_get("", "employee_name")?,
                employee_code: row.try_get("", "employee_code")?,
                policy_id: row.try_get("", "policy_id")?,
                preferences: row.try_get("", "preferences")?,
                created_at: row.try_get("", "created_at")?,
                requested_amount: row.try_get("", "requested_amount")?,
                currency: row.try_get("", "currency")?,
                purpose: row.try_get("", "purpose")?,
                employee_notes: row.try_get("", "employee_notes")?,
                state: row.try_get("", "state")?,
                version: row.try_get("", "version")?,
                workflow_instance_id: row.try_get("", "workflow_instance_id")?,
                current_step_id: row.try_get("", "current_step_id")?,
                is_final_approval_step: row.try_get("", "is_final_approval_step")?,
                decision_reason: row.try_get("", "decision_reason")?,
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
    let metadata = store::one(tx, "SELECT e.first_name || ' ' || e.last_name AS employee_name,e.employee_code,r.purpose,COALESCE((SELECT SUM(p.amount) FROM loan_posting p WHERE p.tenant_id=a.tenant_id AND p.loan_id=a.id AND p.kind='DISBURSEMENT' AND NOT EXISTS(SELECT 1 FROM loan_posting x WHERE x.tenant_id=p.tenant_id AND x.reversal_of=p.id)),0)::text AS disbursed,COALESCE((SELECT SUM(p.amount) FROM loan_posting p WHERE p.tenant_id=a.tenant_id AND p.loan_id=a.id AND p.kind='REPAYMENT' AND NOT EXISTS(SELECT 1 FROM loan_posting x WHERE x.tenant_id=p.tenant_id AND x.reversal_of=p.id)),0)::text AS repaid,COALESCE((SELECT SUM(principal_delta+interest_delta) FROM loan_ledger_entry l WHERE l.tenant_id=a.tenant_id AND l.loan_id=a.id),0)::text AS outstanding,COALESCE((SELECT SUM(remaining_amount) FROM loan_unapplied_credit c WHERE c.tenant_id=a.tenant_id AND c.loan_id=a.id),0)::text AS unapplied_credit FROM loan_account a JOIN employee e ON e.tenant_id=a.tenant_id AND e.id=a.employee_id JOIN loan_request r ON r.tenant_id=a.tenant_id AND r.id=a.request_id WHERE a.tenant_id=$1 AND a.id=$2",vec![actor.tenant_id.into(),id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    Ok(LoanAccountView {
        id,
        employee_id: account.employee_id,
        request_id: account.request_id,
        loan_number: account.loan_number,
        employee_name: metadata.try_get("", "employee_name")?,
        employee_code: metadata.try_get("", "employee_code")?,
        purpose: metadata.try_get("", "purpose")?,
        disbursed: metadata.try_get("", "disbursed")?,
        repaid: metadata.try_get("", "repaid")?,
        outstanding: metadata.try_get("", "outstanding")?,
        unapplied_credit: metadata.try_get("", "unapplied_credit")?,
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
    let nodes=store::all(tx,"WITH payments AS (SELECT id,'DISBURSEMENT'::text AS kind,amount::text AS amount,value_date,method,external_reference,evidence->>'resignationExceptionReason' AS exception_reason,'0'::text AS unapplied_credit FROM loan_disbursement WHERE tenant_id=$1 AND loan_id=$2 UNION ALL SELECT r.id,'RECEIPT',r.amount::text,r.value_date,r.method,r.external_reference,NULL::text AS exception_reason,COALESCE((SELECT SUM(remaining_amount) FROM loan_unapplied_credit c WHERE c.tenant_id=r.tenant_id AND c.loan_id=r.loan_id AND c.receipt_id=r.id),0)::text FROM loan_receipt r WHERE r.tenant_id=$1 AND r.loan_id=$2) SELECT * FROM payments WHERE ($3::uuid IS NULL OR id>$3) ORDER BY id LIMIT $4",vec![actor.tenant_id.into(),loan.into(),after.into(),(i64::from(first)+1).into()]).await?.into_iter().map(|row|Ok(LoanPaymentView{id:row.try_get("","id")?,kind:row.try_get("","kind")?,amount:row.try_get("","amount")?,value_date:row.try_get("","value_date")?,method:row.try_get("","method")?,external_reference:row.try_get("","external_reference")?,unapplied_credit:row.try_get("","unapplied_credit")?,resignation_exception_reason:if actor.scope==kabipay_common::context::ScopeType::All { row.try_get("","exception_reason")? } else { None }})).collect::<LoanResult<Vec<_>>>()?;
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
