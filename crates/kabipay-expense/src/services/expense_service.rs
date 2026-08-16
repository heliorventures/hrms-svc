//! Tenant-scoped SeaORM queries and commands for expenses.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::workflow_approval;
use kabipay_common::workflow_current_step;
use kabipay_common::workflow_inbox::{
    self, NoWorkflowInstanceGate, WorkflowActorCheckMode,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::user_role;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0015_expense::{expense, expense_category, expense_policy};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use kabipay_db_entities::tenant::d0033_travel_request::travel_request;
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::str::FromStr;
use uuid::Uuid;

pub async fn list_categories(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<expense_category::Model>> {
    let limit = limit.clamp(1, 200);
    expense_category::Entity::find()
        .filter(expense_category::Column::TenantId.eq(tenant_id))
        .filter(expense_category::Column::IsDeleted.eq(false))
        .order_by_asc(expense_category::Column::Code)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn get_expense_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    category_id: Uuid,
) -> KabiPayResult<expense_category::Model> {
    expense_category::Entity::find()
        .filter(expense_category::Column::Id.eq(category_id))
        .filter(expense_category::Column::TenantId.eq(tenant_id))
        .filter(expense_category::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense_category",
            id: category_id.to_string(),
        })
}

pub async fn list_expenses(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope_filter: &EmployeeScopeFilter,
) -> KabiPayResult<Vec<expense::Model>> {
    let limit = limit.clamp(1, 200);
    match scope_filter {
        EmployeeScopeFilter::Empty => return Ok(vec![]),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(vec![]),
        _ => {}
    }
    let mut q = expense::Entity::find()
        .filter(expense::Column::TenantId.eq(tenant_id))
        .filter(expense::Column::IsDeleted.eq(false));
    if let EmployeeScopeFilter::EmployeeIds(ids) = scope_filter {
        q = q.filter(expense::Column::EmployeeId.is_in(ids.clone()));
    }
    q.order_by_desc(expense::Column::SubmittedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Matches `workflow.entity_type` / `workflow_instance.entity_type` for expense (M32).
pub const WF_ENTITY_EXPENSE: &str = "EXPENSE";

const WF_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
const WF_STATUS_COMPLETED: &str = "COMPLETED";
const WF_STATUS_CANCELLED: &str = "CANCELLED";
const WF_ACTION_APPROVE: &str = "APPROVE";
const WF_ACTION_REJECT: &str = "REJECT";

/// Submit a new expense claim in `PENDING` status; validates category belongs to the tenant.
/// Optional `travel_request_id` links the claim to that employee’s trip.
///
/// When the tenant defines an active **`EXPENSE`** workflow with ≥1 step, inserts
/// **`workflow_instance`** (**M32**) and sets **`expense.workflow_instance_id`**.
pub async fn submit_expense(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    logged_in_user_id: Uuid,
    expense_category_id: Uuid,
    amount: Decimal,
    currency: &str,
    expense_date: NaiveDate,
    title: &str,
    travel_request_id: Option<Uuid>,
    receipt_file_storage_id: Option<Uuid>,
) -> KabiPayResult<expense::Model> {
    if amount <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "amount must be greater than zero".into(),
        ));
    }
    let currency = normalize_currency_code(currency)?;

    let txn = db.begin().await?;

    let cat = expense_category::Entity::find()
        .filter(expense_category::Column::Id.eq(expense_category_id))
        .filter(expense_category::Column::TenantId.eq(tenant_id))
        .filter(expense_category::Column::IsDeleted.eq(false))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense_category",
            id: expense_category_id.to_string(),
        })?;

    let constraints = resolve_expense_submit_constraints(
        &txn,
        tenant_id,
        expense_category_id,
        employee_id,
        cat.max_amount_per_claim,
    )
    .await?;

    if let Some(cap) = constraints.max_amount_per_claim {
        if amount > cap {
            return Err(KabiPayError::BusinessRule {
                code: "EXPENSE_CLAIM_LIMIT_EXCEEDED",
                message: format!("Amount exceeds the permitted category limit ({cap})."),
            });
        }
    }

    if let Some(ml) = constraints.limit_per_month {
        let (ms, me) = expense_month_bounds(expense_date);
        let already = sum_month_claimed_for_category(
            &txn,
            tenant_id,
            employee_id,
            expense_category_id,
            ms,
            me,
        )
        .await?;
        if already + amount > ml {
            return Err(KabiPayError::BusinessRule {
                code: "EXPENSE_MONTHLY_LIMIT_EXCEEDED",
                message: format!(
                    "This claim would exceed the monthly category limit ({ml}; already claimed {already})."
                ),
            });
        }
    }

    if constraints.receipt_required {
        let fid = receipt_file_storage_id.ok_or_else(|| {
            KabiPayError::Validation(
                "a receipt attachment is required for this category/policy".into(),
            )
        })?;
        assert_receipt_file_owned(&txn, tenant_id, fid, logged_in_user_id).await?;
    } else if let Some(fid) = receipt_file_storage_id {
        assert_receipt_file_owned(&txn, tenant_id, fid, logged_in_user_id).await?;
    }

    if let Some(tid) = travel_request_id {
        let t = travel_request::Entity::find()
            .filter(travel_request::Column::Id.eq(tid))
            .filter(travel_request::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "travel_request",
                id: tid.to_string(),
            })?;
        if t.employee_id != employee_id {
            return Err(KabiPayError::Validation(
                "travel request must belong to the submitting employee".into(),
            ));
        }
    }

    let id = Uuid::new_v4();
    let now = Utc::now();
    let am = expense::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        expense_category_id: Set(expense_category_id),
        amount: Set(amount),
        currency: Set(currency),
        expense_date: Set(expense_date),
        title: Set(title.to_string()),
        status: Set("PENDING".into()),
        approved_amount: Set(None),
        payment_status: Set(PAYMENT_STATUS_NONE.into()),
        paid_at: Set(None),
        payment_reference: Set(None),
        receipt_file_storage_id: Set(receipt_file_storage_id),
        travel_request_id: Set(travel_request_id),
        rejection_reason: Set(None),
        approved_by: Set(None),
        workflow_instance_id: Set(None),
        submitted_at: Set(now),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(&txn).await?;

    try_attach_expense_workflow(&txn, tenant_id, id, now).await?;

    txn.commit().await?;

    expense::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted expense not found".into()))
}

async fn load_expense_workflow_first_step(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
) -> KabiPayResult<Option<(workflow::Model, Uuid)>> {
    let wf = workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_EXPENSE))
        .order_by_asc(workflow::Column::Name)
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(wf) = wf else {
        return Ok(None);
    };
    let step = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(wf.id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(step) = step else {
        return Ok(None);
    };
    Ok(Some((wf, step.id)))
}

async fn try_attach_expense_workflow(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    expense_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let Some((wf, first_step_id)) = load_expense_workflow_first_step(txn, tenant_id).await? else {
        return Ok(());
    };
    let inst_id = Uuid::new_v4();
    let inst = workflow_instance::ActiveModel {
        id: Set(inst_id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(wf.id),
        entity_type: Set(WF_ENTITY_EXPENSE.into()),
        entity_id: Set(expense_id),
        status: Set(WF_STATUS_IN_PROGRESS.into()),
        current_step_id: Set(Some(first_step_id)),
        created_at: Set(now),
        completed_at: Set(None),
        updated_at: Set(now),
    };
    inst.insert(txn).await.map_err(KabiPayError::from)?;

    let mut am_req: expense::ActiveModel = expense::Entity::find_by_id(expense_id)
        .one(txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("expense missing after insert".into()))?
        .into();
    am_req.workflow_instance_id = Set(Some(inst_id));
    am_req.update(txn).await.map_err(KabiPayError::from)?;
    Ok(())
}

/// Parse a decimal from string (GraphQL) into `Decimal`.
pub fn parse_amount(s: &str) -> KabiPayResult<Decimal> {
    Decimal::from_str(s.trim())
        .map_err(|_| KabiPayError::Validation("invalid amount; must be a decimal string".into()))
}

/// Normalize the ISO-style currency code accepted at the API boundary.
pub fn normalize_currency_code(raw: &str) -> KabiPayResult<String> {
    let normalized = raw.trim().to_ascii_uppercase();
    if normalized.len() != 3 || !normalized.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(KabiPayError::Validation(
            "currency must be a 3-letter ISO code".into(),
        ));
    }
    Ok(normalized)
}

#[cfg(test)]
mod currency_tests {
    use super::normalize_currency_code;

    #[test]
    fn normalizes_lowercase_currency_codes() {
        assert_eq!(
            normalize_currency_code(" inr ").expect("valid currency code"),
            "INR"
        );
    }

    #[test]
    fn rejects_partial_or_non_alpha_currency_codes() {
        assert!(normalize_currency_code("").is_err());
        assert!(normalize_currency_code("IN").is_err());
        assert!(normalize_currency_code("IN1").is_err());
    }
}

const STATUS_PENDING: &str = "PENDING";
const STATUS_APPROVED: &str = "APPROVED";
const STATUS_PARTIAL_APPROVED: &str = "PARTIAL_APPROVED";
const STATUS_REJECTED: &str = "REJECTED";

/// Configured `workflow_step.step_name` when **`PENDING`** and the linked instance is **`IN_PROGRESS`**.
pub async fn resolve_expense_pending_approval_stage(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: &str,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<Option<String>> {
    workflow_inbox::pending_workflow_step_title(db, tenant_id, status, STATUS_PENDING, workflow_instance_id)
        .await
}

/// Whether **this** user satisfies the workflow step actor rule or legacy **`expense:approve`** on claims without workflows.
pub async fn expense_viewer_may_approve(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    viewer_user_id: Uuid,
    status: &str,
    subject_employee_id: Uuid,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<bool> {
    workflow_inbox::viewer_may_approve_pending_row(
        db,
        tenant_id,
        viewer_user_id,
        subject_employee_id,
        status,
        STATUS_PENDING,
        workflow_instance_id,
        WorkflowActorCheckMode::Standard,
        NoWorkflowInstanceGate::ResourceAction {
            resource: "expense",
            action: "approve",
        },
    )
    .await
}

pub const PAYMENT_STATUS_NONE: &str = "NONE";
pub const PAYMENT_STATUS_PENDING: &str = "PENDING_PAYMENT";
pub const PAYMENT_STATUS_PAID: &str = "PAID";
pub const PAYMENT_STATUS_FAILED: &str = "FAILED";
pub const PAYMENT_STATUS_ON_HOLD: &str = "ON_HOLD";

const POLICY_ALL: &str = "ALL";
const POLICY_DEPT: &str = "DEPARTMENT";
const POLICY_DES: &str = "DESIGNATION";
const POLICY_ROLE: &str = "ROLE";

fn expense_month_bounds(d: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first = NaiveDate::from_ymd_opt(d.year(), d.month(), 1)
        .expect("valid month first day");
    let (ny, nm) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    let next_first =
        NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid next month first day");
    let last = next_first
        .pred_opt()
        .expect("day before first of month exists");
    (first, last)
}

fn policy_specificity_score(applicable_to: &str) -> i32 {
    match applicable_to {
        POLICY_DEPT => 4,
        POLICY_DES => 3,
        POLICY_ROLE => 2,
        _ => 1,
    }
}

fn policy_applies_to_employee(
    p: &expense_policy::Model,
    emp: &employee::Model,
    user_role_ids: &[Uuid],
) -> bool {
    match p.applicable_to.as_str() {
        POLICY_ALL => true,
        POLICY_DEPT => p
            .department_id
            .is_some_and(|d| Some(d) == emp.department_id),
        POLICY_DES => p
            .designation_id
            .is_some_and(|d| Some(d) == emp.designation_id),
        POLICY_ROLE => p
            .role_id
            .is_some_and(|rid| user_role_ids.iter().any(|u| *u == rid)),
        _ => false,
    }
}

/// Effective caps and receipt rule for one employee × category (best-matching policy tier).
pub struct ExpenseSubmitConstraints {
    pub receipt_required: bool,
    pub max_amount_per_claim: Option<Decimal>,
    /// Minimum non-zero **per-day** limit from the winning policy tier (informational; also folded into `max_amount_per_claim` when it tightens the single-claim ceiling).
    pub limit_per_day: Option<Decimal>,
    pub limit_per_month: Option<Decimal>,
}

pub async fn resolve_expense_submit_constraints(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    category_id: Uuid,
    employee_id: Uuid,
    category_max: Option<Decimal>,
) -> KabiPayResult<ExpenseSubmitConstraints> {
    let emp = employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;

    let user_roles: Vec<Uuid> = if let Some(uid) = emp.user_id {
        user_role::Entity::find()
            .filter(user_role::Column::UserId.eq(uid))
            .all(db)
            .await
            .map_err(KabiPayError::from)?
            .into_iter()
            .map(|r| r.role_id)
            .collect()
    } else {
        vec![]
    };

    let policies = expense_policy::Entity::find()
        .filter(expense_policy::Column::TenantId.eq(tenant_id))
        .filter(expense_policy::Column::ExpenseCategoryId.eq(category_id))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if policies.is_empty() {
        return Ok(ExpenseSubmitConstraints {
            receipt_required: false,
            max_amount_per_claim: category_max,
            limit_per_day: None,
            limit_per_month: None,
        });
    }

    let applicable: Vec<&expense_policy::Model> = policies
        .iter()
        .filter(|p| policy_applies_to_employee(p, &emp, &user_roles))
        .collect();

    let tier_models: Vec<&expense_policy::Model> = if applicable.is_empty() {
        policies.iter().filter(|p| p.applicable_to == POLICY_ALL).collect()
    } else {
        let best = applicable
            .iter()
            .map(|p| policy_specificity_score(p.applicable_to.as_str()))
            .max()
            .unwrap_or(1);
        applicable
            .into_iter()
            .filter(|p| policy_specificity_score(p.applicable_to.as_str()) == best)
            .collect()
    };

    if tier_models.is_empty() {
        return Ok(ExpenseSubmitConstraints {
            receipt_required: false,
            max_amount_per_claim: category_max,
            limit_per_day: None,
            limit_per_month: None,
        });
    }

    let receipt_required = tier_models.iter().any(|p| p.receipt_required);

    let mut caps: Vec<Decimal> = Vec::new();
    if let Some(c) = category_max.filter(|x| *x > Decimal::ZERO) {
        caps.push(c);
    }
    for p in &tier_models {
        if let Some(x) = p.max_amount_per_claim.filter(|v| *v > Decimal::ZERO) {
            caps.push(x);
        }
        if let Some(x) = p.limit_per_day.filter(|v| *v > Decimal::ZERO) {
            caps.push(x);
        }
    }
    let max_amount_per_claim = caps.into_iter().min();

    let month_limits: Vec<Decimal> = tier_models
        .iter()
        .filter_map(|p| p.limit_per_month.filter(|v| *v > Decimal::ZERO))
        .collect();
    let limit_per_month = month_limits.into_iter().min();

    let day_limits: Vec<Decimal> = tier_models
        .iter()
        .filter_map(|p| p.limit_per_day.filter(|v| *v > Decimal::ZERO))
        .collect();
    let limit_per_day = day_limits.into_iter().min();

    Ok(ExpenseSubmitConstraints {
        receipt_required,
        max_amount_per_claim,
        limit_per_day,
        limit_per_month,
    })
}

async fn sum_month_claimed_for_category(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    employee_id: Uuid,
    category_id: Uuid,
    month_start: NaiveDate,
    month_end: NaiveDate,
) -> KabiPayResult<Decimal> {
    let rows = expense::Entity::find()
        .filter(expense::Column::TenantId.eq(tenant_id))
        .filter(expense::Column::EmployeeId.eq(employee_id))
        .filter(expense::Column::ExpenseCategoryId.eq(category_id))
        .filter(expense::Column::IsDeleted.eq(false))
        .filter(expense::Column::ExpenseDate.gte(month_start))
        .filter(expense::Column::ExpenseDate.lte(month_end))
        .filter(expense::Column::Status.is_in(vec![
            STATUS_PENDING.to_string(),
            STATUS_APPROVED.to_string(),
            STATUS_PARTIAL_APPROVED.to_string(),
        ]))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.iter().map(|e| e.amount).sum())
}

async fn assert_receipt_file_owned(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    file_id: Uuid,
    uploader_user_id: Uuid,
) -> KabiPayResult<()> {
    let f = file_storage::Entity::find_by_id(file_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "file_storage",
            id: file_id.to_string(),
        })?;
    if f.uploaded_by != Some(uploader_user_id) {
        return Err(KabiPayError::Validation(
            "receipt file must be uploaded by the submitting user".into(),
        ));
    }
    Ok(())
}

/// Matches **`outbox_event.status`** (`PENDING` until worker processes — **M7**).
const OUTBOX_STATUS_PENDING: &str = "PENDING";

/// Financial sign-off: **APPROVED** / **PARTIAL_APPROVED**, **`approved_amount`**, **`PENDING_PAYMENT`**, and outbox enqueue (**M33**).
async fn finalize_expense_approval(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    expense_id: Uuid,
    approver_user_id: Uuid,
    approved_financial: Decimal,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<expense::Model> {
    let m = expense::Entity::find()
        .filter(expense::Column::Id.eq(expense_id))
        .filter(expense::Column::TenantId.eq(tenant_id))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense",
            id: expense_id.to_string(),
        })?;

    let claim = m.amount.normalize();
    let fin = approved_financial.normalize();
    if fin <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "approved amount must be greater than zero".into(),
        ));
    }
    if fin > claim {
        return Err(KabiPayError::Validation(
            "approved amount cannot exceed the claimed amount".into(),
        ));
    }
    let status_fin = if fin < claim {
        STATUS_PARTIAL_APPROVED
    } else {
        STATUS_APPROVED
    };

    let mut am: expense::ActiveModel = m.into();
    am.status = Set(status_fin.into());
    am.rejection_reason = Set(None);
    am.approved_by = Set(Some(approver_user_id));
    am.approved_amount = Set(Some(fin));
    am.payment_status = Set(PAYMENT_STATUS_PENDING.into());
    am.updated_at = Set(now);
    am.update(txn).await?;
    let out = expense::Entity::find()
        .filter(expense::Column::Id.eq(expense_id))
        .filter(expense::Column::TenantId.eq(tenant_id))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated expense not found".into()))?;

    let payload = serde_json::json!({
        "schema_version": 2,
        "expense_id": out.id,
        "employee_id": out.employee_id,
        "expense_category_id": out.expense_category_id,
        "approver_user_id": approver_user_id,
        "amount": out.amount.normalize().to_string(),
        "approved_amount": out.approved_amount.map(|v| v.normalize().to_string()),
        "currency": out.currency,
        "expense_date": out.expense_date.to_string(),
        "status": out.status,
        "payment_status": out.payment_status,
        "title": out.title,
    });
    let ob = outbox_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        aggregate_type: Set("expense".into()),
        aggregate_id: Set(expense_id),
        event_type: Set("expense.approved".into()),
        payload: Set(payload.into()),
        status: Set(OUTBOX_STATUS_PENDING.into()),
        retry_count: Set(0),
        last_error: Set(None),
        created_at: Set(now),
        processed_at: Set(None),
        claimed_at: Set(None),
    };
    ob.insert(txn).await.map_err(KabiPayError::from)?;
    Ok(out)
}

fn resolve_final_approved_amount(
    claim_amount: Decimal,
    approved_amount: Option<Decimal>,
) -> KabiPayResult<Decimal> {
    let claim = claim_amount.normalize();
    match approved_amount {
        None => Ok(claim),
        Some(x) => {
            let f = x.normalize();
            if f <= Decimal::ZERO {
                return Err(KabiPayError::Validation(
                    "approved amount must be greater than zero".into(),
                ));
            }
            if f > claim {
                return Err(KabiPayError::Validation(
                    "approved amount cannot exceed the claimed amount".into(),
                ));
            }
            Ok(f)
        }
    }
}

/// Approve routing: **`workflow_instance`** multi-step (**M32**) — intermediate steps advance the
/// instance without setting **APPROVED**; legacy **PENDING** with no workflow is single-step approve.
pub async fn approve_expense(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    expense_id: Uuid,
    approver_user_id: Uuid,
    approved_amount: Option<Decimal>,
) -> KabiPayResult<expense::Model> {
    let txn = db.begin().await?;
    let model = load_pending_expense_conn(&txn, tenant_id, expense_id).await?;
    let now = Utc::now();

    if let Some(inst_id) = model.workflow_instance_id {
        let mut inst = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Validation("expense workflow_instance not found".into()))?;
        if inst.status != WF_STATUS_IN_PROGRESS {
            return Err(KabiPayError::Validation(
                "workflow instance is not in progress — cannot approve this expense".into(),
            ));
        }
        inst = workflow_current_step::ensure_workflow_instance_current_step_repaired(
            &txn, tenant_id, &inst, now,
        )
        .await?;
        let cur_step_id = inst.current_step_id.ok_or_else(|| {
            KabiPayError::Validation("workflow instance has no current step".into())
        })?;
        let cur_step = workflow_step::Entity::find_by_id(cur_step_id)
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Validation("workflow step not found".into()))?;

        workflow_approval::assert_workflow_step_actor(
            &txn,
            tenant_id,
            approver_user_id,
            model.employee_id,
            &cur_step,
        )
        .await?;

        let act = workflow_action::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            instance_id: Set(inst_id),
            workflow_step_id: Set(cur_step_id),
            performed_by: Set(Some(approver_user_id)),
            action: Set(WF_ACTION_APPROVE.into()),
            remarks: Set(None),
            acted_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        };
        act.insert(&txn).await?;

        let next_step = workflow_step::Entity::find()
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .filter(workflow_step::Column::WorkflowId.eq(inst.workflow_id))
            .filter(workflow_step::Column::SequenceOrder.gt(cur_step.sequence_order))
            .order_by_asc(workflow_step::Column::SequenceOrder)
            .one(&txn)
            .await?;

        if let Some(next) = next_step {
            if approved_amount.is_some() {
                return Err(KabiPayError::Validation(
                    "approved financial amount applies only on the final approval step".into(),
                ));
            }
            let mut am_inst: workflow_instance::ActiveModel = inst.into();
            am_inst.current_step_id = Set(Some(next.id));
            am_inst.updated_at = Set(now);
            am_inst.update(&txn).await?;
            txn.commit().await?;
            return expense::Entity::find_by_id(expense_id)
                .one(db)
                .await?
                .ok_or_else(|| KabiPayError::Internal("expense missing after workflow step".into()));
        }

        let mut am_inst: workflow_instance::ActiveModel = inst.into();
        am_inst.status = Set(WF_STATUS_COMPLETED.into());
        am_inst.current_step_id = Set(None);
        am_inst.completed_at = Set(Some(now));
        am_inst.updated_at = Set(now);
        am_inst.update(&txn).await?;

        let fin = resolve_final_approved_amount(model.amount, approved_amount)?;
        finalize_expense_approval(&txn, tenant_id, expense_id, approver_user_id, fin, now).await?;
    } else {
        if !workflow_approval::user_has_permission_via_roles(
            &txn,
            tenant_id,
            approver_user_id,
            "expense",
            "approve",
        )
        .await?
        {
            return Err(KabiPayError::Forbidden(
                "only users with expense approval permission may approve claims without a workflow"
                    .into(),
            ));
        }
        let fin = resolve_final_approved_amount(model.amount, approved_amount)?;
        finalize_expense_approval(&txn, tenant_id, expense_id, approver_user_id, fin, now).await?;
    }

    txn.commit().await?;

    let out = expense::Entity::find_by_id(expense_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("expense missing after approve".into()))?;

    let note = match out.status.as_str() {
        STATUS_PARTIAL_APPROVED => format!(
            "Partially approved — reimbursable amount {} {}.",
            out.approved_amount
                .map(|d| d.to_string())
                .unwrap_or_else(|| "?".into()),
            out.currency
        ),
        _ => "Approved.".to_string(),
    };
    expense_notify_employee(
        db,
        tenant_id,
        out.employee_id,
        "Expense approved",
        &format!("Your expense claim \"{}\" {}", out.title, note),
    )
    .await;
    Ok(out)
}

pub async fn reject_expense(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    expense_id: Uuid,
    rejector_user_id: Uuid,
    rejection_reason: Option<String>,
) -> KabiPayResult<expense::Model> {
    let txn = db.begin().await?;
    let model = load_pending_expense_conn(&txn, tenant_id, expense_id).await?;

    if model.workflow_instance_id.is_none() {
        if !workflow_approval::user_has_permission_via_roles(
            &txn,
            tenant_id,
            rejector_user_id,
            "expense",
            "approve",
        )
        .await?
        {
            return Err(KabiPayError::Forbidden(
                "only users with expense approval permission may reject claims without a workflow"
                    .into(),
            ));
        }
    }

    if let Some(inst_id) = model.workflow_instance_id {
        if let Some(mut inst) = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
        {
            if inst.status == WF_STATUS_IN_PROGRESS {
                let now = Utc::now();
                inst = workflow_current_step::ensure_workflow_instance_current_step_repaired(
                    &txn, tenant_id, &inst, now,
                )
                .await?;
                if let Some(step_id) = inst.current_step_id {
                    let st = workflow_step::Entity::find_by_id(step_id)
                        .filter(workflow_step::Column::TenantId.eq(tenant_id))
                        .one(&txn)
                        .await?
                        .ok_or_else(|| KabiPayError::Validation("workflow step not found".into()))?;
                    workflow_approval::assert_workflow_step_actor(
                        &txn,
                        tenant_id,
                        rejector_user_id,
                        model.employee_id,
                        &st,
                    )
                    .await?;
                    let act = workflow_action::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        tenant_id: Set(tenant_id),
                        instance_id: Set(inst_id),
                        workflow_step_id: Set(step_id),
                        performed_by: Set(Some(rejector_user_id)),
                        action: Set(WF_ACTION_REJECT.into()),
                        remarks: Set(rejection_reason.clone()),
                        acted_at: Set(now),
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    act.insert(&txn).await?;
                }
                let mut am_inst: workflow_instance::ActiveModel = inst.into();
                am_inst.status = Set(WF_STATUS_CANCELLED.into());
                am_inst.completed_at = Set(Some(now));
                am_inst.updated_at = Set(now);
                am_inst.update(&txn).await?;
            }
        }
    }

    let now = Utc::now();
    let mut am: expense::ActiveModel = model.into();
    am.status = Set(STATUS_REJECTED.into());
    am.rejection_reason = Set(rejection_reason.clone());
    am.approved_by = Set(None);
    am.approved_amount = Set(None);
    am.payment_status = Set(PAYMENT_STATUS_NONE.into());
    am.paid_at = Set(None);
    am.payment_reference = Set(None);
    am.updated_at = Set(now);
    am.update(&txn).await?;

    let out = expense::Entity::find_by_id(expense_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated expense not found".into()))?;

    txn.commit().await?;

    let msg = format!(
        "Your expense claim \"{}\" was rejected.{}",
        out.title,
        match &out.rejection_reason {
            Some(s) if !s.is_empty() => format!(" Reason: {s}"),
            _ => String::new(),
        }
    );
    expense_notify_employee(db, tenant_id, out.employee_id, "Expense rejected", &msg).await;
    Ok(out)
}

async fn load_pending_expense_conn(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    expense_id: Uuid,
) -> KabiPayResult<expense::Model> {
    let m = expense::Entity::find()
        .filter(expense::Column::Id.eq(expense_id))
        .filter(expense::Column::TenantId.eq(tenant_id))
        .filter(expense::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense",
            id: expense_id.to_string(),
        })?;
    if m.status != STATUS_PENDING {
        return Err(KabiPayError::Validation(
            "only PENDING expenses can be approved or rejected".into(),
        ));
    }
    Ok(m)
}

/// Upsert **`expense_category`** (tenant master data for claim types). Caller enforces **`expense:manage`**.
pub async fn upsert_expense_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: &str,
    code: &str,
    max_amount_per_claim: Option<Decimal>,
) -> KabiPayResult<expense_category::Model> {
    let name = name.trim();
    let code = code.trim().to_ascii_uppercase();
    if name.is_empty() || code.is_empty() {
        return Err(KabiPayError::Validation(
            "category name and code are required".into(),
        ));
    }
    let now = Utc::now();
    if let Some(mid) = id {
        let m = expense_category::Entity::find()
            .filter(expense_category::Column::Id.eq(mid))
            .filter(expense_category::Column::TenantId.eq(tenant_id))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "expense_category",
                id: mid.to_string(),
            })?;
        if m.is_deleted {
            return Err(KabiPayError::Validation(
                "cannot update a deleted expense category".into(),
            ));
        }
        let dup = expense_category::Entity::find()
            .filter(expense_category::Column::TenantId.eq(tenant_id))
            .filter(expense_category::Column::Code.eq(code.clone()))
            .filter(expense_category::Column::Id.ne(mid))
            .filter(expense_category::Column::IsDeleted.eq(false))
            .one(db)
            .await?;
        if dup.is_some() {
            return Err(KabiPayError::Validation(format!(
                "expense category code `{code}` is already in use"
            )));
        }
        let mut am: expense_category::ActiveModel = m.into();
        am.name = Set(name.to_string());
        am.code = Set(code.clone());
        am.max_amount_per_claim = Set(max_amount_per_claim);
        am.updated_at = Set(now);
        am.update(db).await?;
        expense_category::Entity::find_by_id(mid)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated expense category not found".into()))
    } else {
        let exists = expense_category::Entity::find()
            .filter(expense_category::Column::TenantId.eq(tenant_id))
            .filter(expense_category::Column::Code.eq(code.clone()))
            .filter(expense_category::Column::IsDeleted.eq(false))
            .one(db)
            .await?;
        if exists.is_some() {
            return Err(KabiPayError::Validation(format!(
                "expense category code `{code}` is already in use"
            )));
        }
        let pk = Uuid::new_v4();
        let am = expense_category::ActiveModel {
            id: Set(pk),
            tenant_id: Set(tenant_id),
            name: Set(name.to_string()),
            code: Set(code),
            max_amount_per_claim: Set(max_amount_per_claim),
            is_deleted: Set(false),
            deleted_at: Set(None),
            deleted_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await?;
        expense_category::Entity::find_by_id(pk)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("inserted expense category not found".into()))
    }
}

pub async fn delete_expense_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    acting_user_id: Uuid,
    id: Uuid,
) -> KabiPayResult<()> {
    let m = expense_category::Entity::find()
        .filter(expense_category::Column::Id.eq(id))
        .filter(expense_category::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense_category",
            id: id.to_string(),
        })?;
    if m.is_deleted {
        return Ok(());
    }
    let now = Utc::now();
    let mut am: expense_category::ActiveModel = m.into();
    am.is_deleted = Set(true);
    am.deleted_at = Set(Some(now));
    am.deleted_by = Set(Some(acting_user_id));
    am.updated_at = Set(now);
    am.update(db).await?;
    Ok(())
}

pub fn normalize_expense_payment_status_wire(s: &str) -> KabiPayResult<&'static str> {
    match s.trim() {
        "NONE" | "none" => Ok(PAYMENT_STATUS_NONE),
        "PENDING_PAYMENT" | "pending_payment" | "PendingPayment" => Ok(PAYMENT_STATUS_PENDING),
        "PAID" | "paid" => Ok(PAYMENT_STATUS_PAID),
        "FAILED" | "failed" => Ok(PAYMENT_STATUS_FAILED),
        "ON_HOLD" | "on_hold" | "OnHold" => Ok(PAYMENT_STATUS_ON_HOLD),
        _ => Err(KabiPayError::Validation(
            "unknown expense payment status; expected NONE | PENDING_PAYMENT | PAID | FAILED | ON_HOLD"
                .into(),
        )),
    }
}

pub async fn mark_expense_payment_status(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    expense_id: Uuid,
    new_payment_status: &str,
    payment_reference: Option<&str>,
) -> KabiPayResult<expense::Model> {
    let pst = normalize_expense_payment_status_wire(new_payment_status)?;
    let m = expense::Entity::find()
        .filter(expense::Column::Id.eq(expense_id))
        .filter(expense::Column::TenantId.eq(tenant_id))
        .filter(expense::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense",
            id: expense_id.to_string(),
        })?;
    if m.status != STATUS_APPROVED && m.status != STATUS_PARTIAL_APPROVED {
        return Err(KabiPayError::Validation(
            "payment status can only change for financially approved expense claims".into(),
        ));
    }
    let now = Utc::now();
    let mut am: expense::ActiveModel = m.into();
    am.payment_status = Set(pst.to_string());
    match pst {
        PAYMENT_STATUS_PAID => {
            am.paid_at = Set(Some(now));
            let pref = payment_reference.map(str::trim).filter(|x| !x.is_empty());
            am.payment_reference = Set(pref.map(|s| s.to_string()));
        }
        PAYMENT_STATUS_PENDING => {
            am.paid_at = Set(None);
            am.payment_reference = Set(
                payment_reference
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(|s| s.to_string()),
            );
        }
        PAYMENT_STATUS_NONE => {
            am.paid_at = Set(None);
            am.payment_reference = Set(None);
        }
        PAYMENT_STATUS_FAILED => {
            am.paid_at = Set(None);
            let pref = payment_reference.map(str::trim).filter(|x| !x.is_empty());
            am.payment_reference = Set(pref.map(|s| s.to_string()));
        }
        PAYMENT_STATUS_ON_HOLD => {
            am.paid_at = Set(None);
            let pref = payment_reference.map(str::trim).filter(|x| !x.is_empty());
            am.payment_reference = Set(pref.map(|s| s.to_string()));
        }
        _ => {
            return Err(KabiPayError::Internal(
                "normalized payment status unexpectedly failed to match constants".into(),
            ));
        }
    }
    am.updated_at = Set(now);
    am.update(db).await?;

    expense::Entity::find_by_id(expense_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("expense missing after payment update".into()))
}

fn expense_policy_validate_scope(applicable_to: &str, dept: Option<Uuid>, des: Option<Uuid>, rol: Option<Uuid>) -> KabiPayResult<()> {
    let at = applicable_to.trim();
    match at {
        POLICY_ALL => {
            if dept.is_some() || des.is_some() || rol.is_some() {
                return Err(KabiPayError::Validation(
                    "policy applicable ALL requires department, designation, and role identifiers to be unset"
                        .into(),
                ));
            }
            Ok(())
        }
        POLICY_DEPT => {
            if dept.is_none() || des.is_some() || rol.is_some() {
                return Err(KabiPayError::Validation(
                    "policy applicable DEPARTMENT requires department_id".into(),
                ));
            }
            Ok(())
        }
        POLICY_DES => {
            if des.is_none() || dept.is_some() || rol.is_some() {
                return Err(KabiPayError::Validation(
                    "policy applicable DESIGNATION requires designation_id".into(),
                ));
            }
            Ok(())
        }
        POLICY_ROLE => {
            if rol.is_none() || dept.is_some() || des.is_some() {
                return Err(KabiPayError::Validation(
                    "policy applicable ROLE requires role_id".into(),
                ));
            }
            Ok(())
        }
        _ => Err(KabiPayError::Validation(format!(
            "unsupported applicable_to `{applicable_to}` (use ALL | DEPARTMENT | DESIGNATION | ROLE)"
        ))),
    }
}

pub async fn list_expense_policies_for_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    expense_category_id: Uuid,
) -> KabiPayResult<Vec<expense_policy::Model>> {
    expense_policy::Entity::find()
        .filter(expense_policy::Column::TenantId.eq(tenant_id))
        .filter(expense_policy::Column::ExpenseCategoryId.eq(expense_category_id))
        .order_by_asc(expense_policy::Column::ApplicableTo)
        .order_by_desc(expense_policy::Column::UpdatedAt)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn upsert_expense_policy_admin(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    expense_category_id: Uuid,
    applicable_to: &str,
    department_id: Option<Uuid>,
    designation_id: Option<Uuid>,
    role_id: Option<Uuid>,
    limit_per_day: Option<Decimal>,
    limit_per_month: Option<Decimal>,
    max_amount_per_claim: Option<Decimal>,
    receipt_required: bool,
    approval_required: bool,
) -> KabiPayResult<expense_policy::Model> {
    let at_trim = applicable_to.trim();
    expense_policy_validate_scope(at_trim, department_id, designation_id, role_id)?;

    let _cat = expense_category::Entity::find()
        .filter(expense_category::Column::Id.eq(expense_category_id))
        .filter(expense_category::Column::TenantId.eq(tenant_id))
        .filter(expense_category::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "expense_category",
            id: expense_category_id.to_string(),
        })?;

    let now = Utc::now();
    let at_owned = at_trim.to_string();
    if let Some(pid) = id {
        let m = expense_policy::Entity::find()
            .filter(expense_policy::Column::Id.eq(pid))
            .filter(expense_policy::Column::TenantId.eq(tenant_id))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "expense_policy",
                id: pid.to_string(),
            })?;
        let mut am: expense_policy::ActiveModel = m.into();
        am.expense_category_id = Set(expense_category_id);
        am.limit_per_day = Set(limit_per_day);
        am.limit_per_month = Set(limit_per_month);
        am.receipt_required = Set(receipt_required);
        am.approval_required = Set(approval_required);
        am.applicable_to = Set(at_owned);
        am.department_id = Set(department_id);
        am.designation_id = Set(designation_id);
        am.role_id = Set(role_id);
        am.max_amount_per_claim = Set(max_amount_per_claim);
        am.updated_at = Set(now);
        am.update(db).await?;
        expense_policy::Entity::find_by_id(pid)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated expense policy missing".into()))
    } else {
        let pk = Uuid::new_v4();
        let am = expense_policy::ActiveModel {
            id: Set(pk),
            tenant_id: Set(tenant_id),
            expense_category_id: Set(expense_category_id),
            limit_per_day: Set(limit_per_day),
            limit_per_month: Set(limit_per_month),
            receipt_required: Set(receipt_required),
            approval_required: Set(approval_required),
            applicable_to: Set(at_owned),
            department_id: Set(department_id),
            designation_id: Set(designation_id),
            role_id: Set(role_id),
            max_amount_per_claim: Set(max_amount_per_claim),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await?;
        expense_policy::Entity::find_by_id(pk)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("inserted expense policy missing".into()))
    }
}

pub async fn delete_expense_policy_admin(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    policy_id: Uuid,
) -> KabiPayResult<()> {
    let deleted = expense_policy::Entity::delete_many()
        .filter(expense_policy::Column::Id.eq(policy_id))
        .filter(expense_policy::Column::TenantId.eq(tenant_id))
        .exec(db)
        .await?;
    if deleted.rows_affected == 0 {
        return Err(KabiPayError::NotFound {
            entity: "expense_policy",
            id: policy_id.to_string(),
        });
    }
    Ok(())
}

async fn expense_notify_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    title: &str,
    message: &str,
) {
    let user_id: Option<Uuid> = match employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
    {
        Ok(Some(emp)) => emp.user_id,
        _ => None,
    };
    let Some(user_id) = user_id else {
        return;
    };
    let now = Utc::now();
    let am = notification::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        r#type: Set(Some("EXPENSE".into())),
        title: Set(Some(title.into())),
        message: Set(Some(message.into())),
        action_url: Set(None),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    if let Err(e) = am.insert(db).await {
        tracing::warn!(error = %e, "insert notification (expense) failed");
    }
}
