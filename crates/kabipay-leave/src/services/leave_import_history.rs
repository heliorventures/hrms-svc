//! Aggregate history does not create approved requests or payroll inputs.
use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0011_leave::{leave_balance, leave_type},
    d0091_leave_import_history::leave_import_history,
};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set,
    Statement,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningSnapshot {
    pub year: i32,
    pub as_of: NaiveDate,
    pub carry_forward: Option<String>,
    pub grant: Option<String>,
    pub source_taken: Option<String>,
    pub source_balance: Option<String>,
    pub paid_used: Option<String>,
    pub paid_remaining: Option<String>,
    pub pending: Option<String>,
    pub planned: Option<String>,
    pub ready: bool,
}
pub struct ReconciledOpening {
    pub carry: Decimal,
    pub grant: Decimal,
    pub paid_used: Decimal,
    pub paid_remaining: Decimal,
    pub historical_lwp: Decimal,
}
fn number(value: Option<&str>) -> KabiPayResult<Decimal> {
    Decimal::from_str(
        value.ok_or_else(|| KabiPayError::Validation("leave values are unknown".into()))?,
    )
    .map_err(|_| KabiPayError::Validation("leave values must be decimals".into()))
}
pub fn validate_opening(input: &OpeningSnapshot) -> KabiPayResult<ReconciledOpening> {
    let carry = number(input.carry_forward.as_deref())?;
    let grant = number(input.grant.as_deref())?;
    let taken = number(input.source_taken.as_deref())?;
    let balance = number(input.source_balance.as_deref())?;
    let used = number(input.paid_used.as_deref())?;
    let remaining = number(input.paid_remaining.as_deref())?;
    if !input.ready
        || input.as_of.year() != input.year
        || [carry, grant, taken, used, remaining]
            .iter()
            .any(|v| v.is_sign_negative())
        || carry + grant - taken != balance
        || used != taken.min(carry + grant)
        || remaining != balance.max(Decimal::ZERO)
        || [carry, grant, taken]
            .iter()
            .any(|v| *v > Decimal::from(3660))
    {
        return Err(KabiPayError::Validation(
            "leave opening does not reconcile; no sign or entitlement is inferred".into(),
        ));
    }
    for value in [&input.pending, &input.planned].into_iter().flatten() {
        if number(Some(value))?.is_sign_negative() {
            return Err(KabiPayError::Validation(
                "pending/planned usage cannot be negative".into(),
            ));
        }
    }
    Ok(ReconciledOpening {
        carry,
        grant,
        paid_used: used,
        paid_remaining: remaining,
        historical_lwp: (taken - carry - grant).max(Decimal::ZERO),
    })
}
pub async fn ensure_type<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    code: &str,
    paid: bool,
) -> KabiPayResult<Uuid> {
    if code.trim().is_empty() || code.len() > 32 {
        return Err(KabiPayError::Validation("invalid leave type code".into()));
    }
    let rows = leave_type::Entity::find()
        .filter(leave_type::Column::TenantId.eq(tenant))
        .filter(leave_type::Column::Code.eq(code))
        .all(db)
        .await?;
    if rows.len() > 1
        || rows
            .first()
            .is_some_and(|r| r.is_deleted || r.is_paid != paid)
    {
        return Err(KabiPayError::Validation(
            "leave type conflicts with import configuration".into(),
        ));
    }
    let now = Utc::now();
    let id = if let Some(row) = rows.first() {
        row.id
    } else {
        let id = Uuid::new_v4();
        leave_type::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant),
            name: Set(if paid {
                "Earned Leave"
            } else {
                "Leave Without Pay"
            }
            .into()),
            code: Set(code.into()),
            is_paid: Set(paid),
            carry_forward: Set(paid),
            max_carry_forward_days: Set(None),
            sandwich_rule: Set(false),
            half_day_allowed: Set(true),
            requires_document: Set(false),
            is_deleted: Set(false),
            deleted_at: Set(None),
            deleted_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
        id
    };
    if !paid {
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE leave_policy SET annual_entitlement=NULL,accrual_frequency=NULL,accrual_days=NULL,max_consecutive_days=NULL,updated_at=NOW() WHERE tenant_id=$1 AND leave_type_id=$2",[tenant.into(),id.into()])).await?;
    }
    Ok(id)
}
pub async fn save<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    paid_type: Uuid,
    snapshot: OpeningSnapshot,
    historical_lwp: Decimal,
    source_ref: serde_json::Value,
) -> KabiPayResult<&'static str> {
    if historical_lwp.is_sign_negative() || snapshot.as_of.year() != snapshot.year {
        return Err(KabiPayError::Validation(
            "invalid historical leave attribution".into(),
        ));
    }
    let reconciled = if snapshot.ready {
        Some(validate_opening(&snapshot)?)
    } else {
        None
    };
    if reconciled
        .as_ref()
        .is_some_and(|v| v.historical_lwp != historical_lwp)
    {
        return Err(KabiPayError::Validation(
            "historical LWP conflicts with opening usage".into(),
        ));
    }
    let opening = serde_json::to_value(&snapshot)
        .map_err(|_| KabiPayError::Validation("invalid opening snapshot".into()))?;
    let existing = leave_import_history::Entity::find()
        .filter(leave_import_history::Column::TenantId.eq(tenant))
        .filter(leave_import_history::Column::EmployeeId.eq(employee))
        .filter(leave_import_history::Column::Year.eq(snapshot.year))
        .one(db)
        .await?;
    if let Some(row) = &existing {
        if row.leave_type_id != paid_type {
            return Err(KabiPayError::Validation(
                "imported leave type changed; review the opening reconciliation".into(),
            ));
        }
        if row.opening == opening && row.historical_lwp == historical_lwp {
            return Ok("UNCHANGED");
        }
        let previous: OpeningSnapshot = serde_json::from_value(row.opening.clone())
            .map_err(|_| KabiPayError::Validation("stored leave opening requires review".into()))?;
        let balance = leave_balance::Entity::find()
            .filter(leave_balance::Column::TenantId.eq(tenant))
            .filter(leave_balance::Column::EmployeeId.eq(employee))
            .filter(leave_balance::Column::LeaveTypeId.eq(paid_type))
            .filter(leave_balance::Column::Year.eq(snapshot.year))
            .one(db)
            .await?;
        let unchanged = if previous.ready {
            let original = validate_opening(&previous)?;
            balance.is_some_and(|balance| {
                balance.entitled_days == original.grant
                    && balance.carried_forward_days == original.carry
                    && balance.used_days == original.paid_used
                    && balance.balance_days == original.paid_remaining
                    && balance.pending_days
                        == previous
                            .pending
                            .as_deref()
                            .map(|value| number(Some(value)))
                            .transpose()
                            .ok()
                            .flatten()
                            .unwrap_or_default()
            })
        } else {
            balance.is_none()
        };
        if !unchanged {
            return Err(KabiPayError::Validation(
                "leave balance was adjusted; reviewed reconciliation is required".into(),
            ));
        }
        let activity=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT (SELECT COUNT(*) FROM leave_request WHERE tenant_id=$1 AND employee_id=$2 AND NOT is_deleted)+(SELECT COUNT(*) FROM leave_accrual_log WHERE tenant_id=$1 AND employee_id=$2) AS n",[tenant.into(),employee.into()])).await?
            .ok_or_else(||KabiPayError::Internal("missing leave activity result".into()))?;
        if activity.try_get::<i64>("", "n")? > 0 {
            return Err(KabiPayError::Validation(
                "leave activity requires reviewed opening reconciliation".into(),
            ));
        }
    }
    let now = Utc::now();
    let id = existing.as_ref().map(|r| r.id).unwrap_or_else(Uuid::new_v4);
    let model = leave_import_history::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant),
        employee_id: Set(employee),
        leave_type_id: Set(paid_type),
        year: Set(snapshot.year),
        as_of: Set(snapshot.as_of),
        opening: Set(opening),
        historical_lwp: Set(historical_lwp),
        ready: Set(snapshot.ready),
        source_ref: Set(Some(source_ref)),
        updated_by: Set(actor),
        created_at: Set(existing.as_ref().map(|r| r.created_at).unwrap_or(now)),
        updated_at: Set(now),
    };
    if existing.is_some() {
        model.update(db).await?;
    } else {
        model.insert(db).await?;
    }
    if let Some(value) = reconciled {
        let balance = leave_balance::Entity::find()
            .filter(leave_balance::Column::TenantId.eq(tenant))
            .filter(leave_balance::Column::EmployeeId.eq(employee))
            .filter(leave_balance::Column::LeaveTypeId.eq(paid_type))
            .filter(leave_balance::Column::Year.eq(snapshot.year))
            .one(db)
            .await?;
        if balance.is_some() && existing.is_none() {
            return Err(KabiPayError::Validation(
                "existing balance requires reviewed reconciliation".into(),
            ));
        }
        let model = leave_balance::ActiveModel {
            id: Set(balance.as_ref().map(|r| r.id).unwrap_or_else(Uuid::new_v4)),
            tenant_id: Set(tenant),
            employee_id: Set(employee),
            leave_type_id: Set(paid_type),
            year: Set(snapshot.year),
            entitled_days: Set(value.grant),
            used_days: Set(value.paid_used),
            pending_days: Set(snapshot
                .pending
                .as_deref()
                .map(|v| number(Some(v)))
                .transpose()?
                .unwrap_or_default()),
            carried_forward_days: Set(value.carry),
            balance_days: Set(value.paid_remaining),
            created_at: Set(balance.as_ref().map(|r| r.created_at).unwrap_or(now)),
            updated_at: Set(now),
        };
        if balance.is_some() {
            model.update(db).await?;
        } else {
            model.insert(db).await?;
        }
    }
    Ok(if existing.is_some() {
        "UPDATED"
    } else {
        "CREATED"
    })
}
