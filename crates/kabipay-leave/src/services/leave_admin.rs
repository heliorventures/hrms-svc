//! Tenant admin configuration: leave types, policies, and balances.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0011_leave::{leave_balance, leave_policy, leave_type};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
};
use std::collections::HashSet;
use uuid::Uuid;

fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

fn balance_days_from_components(
    entitled: Decimal,
    carried: Decimal,
    used: Decimal,
    pending: Decimal,
) -> Decimal {
    entitled + carried - used - pending
}

fn compute_balance_days(
    entitled: Decimal,
    carried: Decimal,
    used: Decimal,
    pending: Decimal,
) -> KabiPayResult<Decimal> {
    let v = balance_days_from_components(entitled, carried, used, pending);
    if v < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "balance_days would be negative — check entitled, carried forward, used, and pending"
                .into(),
        ));
    }
    Ok(v)
}

pub async fn list_leave_policies(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<leave_policy::Model>> {
    let limit = limit.clamp(1, 500);
    leave_policy::Entity::find()
        .filter(leave_policy::Column::TenantId.eq(tenant_id))
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn upsert_leave_type(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    code: String,
    is_paid: bool,
    carry_forward: bool,
    max_carry_forward_days: Option<i32>,
    sandwich_rule: bool,
    half_day_allowed: bool,
    requires_document: bool,
) -> KabiPayResult<leave_type::Model> {
    let name = name.trim().to_string();
    let code = normalize_code(&code);
    if name.is_empty() || code.is_empty() {
        return Err(KabiPayError::Validation(
            "leave type name and code are required".into(),
        ));
    }

    let now = Utc::now();

    if let Some(existing_id) = id {
        let found = leave_type::Entity::find_by_id(existing_id)
            .filter(leave_type::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "leave_type",
                id: existing_id.to_string(),
            })?;
        let mut am: leave_type::ActiveModel = found.into();
        am.name = Set(name.clone());
        am.code = Set(code.clone());
        am.is_paid = Set(is_paid);
        am.carry_forward = Set(carry_forward);
        am.max_carry_forward_days = Set(max_carry_forward_days);
        am.sandwich_rule = Set(sandwich_rule);
        am.half_day_allowed = Set(half_day_allowed);
        am.requires_document = Set(requires_document);
        am.updated_at = Set(now);
        let updated = am.update(db).await.map_err(KabiPayError::from)?;
        return Ok(updated);
    }

    let dup = leave_type::Entity::find()
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::Code.eq(&code))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    if dup.is_some() {
        return Err(KabiPayError::Validation(format!(
            "leave type code `{code}` already exists"
        )));
    }

    let new_id = Uuid::new_v4();
    let am = leave_type::ActiveModel {
        id: Set(new_id),
        tenant_id: Set(tenant_id),
        name: Set(name),
        code: Set(code),
        is_paid: Set(is_paid),
        carry_forward: Set(carry_forward),
        max_carry_forward_days: Set(max_carry_forward_days),
        sandwich_rule: Set(sandwich_rule),
        half_day_allowed: Set(half_day_allowed),
        requires_document: Set(requires_document),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    leave_type::Entity::find_by_id(new_id)
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("inserted leave_type not found".into()))
}

pub async fn soft_delete_leave_type(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    leave_type_id: Uuid,
    deleted_by: Option<Uuid>,
) -> KabiPayResult<leave_type::Model> {
    let found = leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: leave_type_id.to_string(),
        })?;
    let now = Utc::now();
    let mut am: leave_type::ActiveModel = found.into();
    am.is_deleted = Set(true);
    am.deleted_at = Set(Some(now));
    am.deleted_by = Set(deleted_by);
    am.updated_at = Set(now);
    Ok(am.update(db).await.map_err(KabiPayError::from)?)
}

pub async fn upsert_leave_policy(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    leave_type_id: Uuid,
    applicable_to: Option<String>,
    annual_entitlement: Option<i32>,
    accrual_frequency: Option<String>,
    accrual_days: Option<Decimal>,
    max_consecutive_days: Option<i32>,
    min_notice_days: Option<i32>,
) -> KabiPayResult<leave_policy::Model> {
    let lt = leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: leave_type_id.to_string(),
        })?;

    let now = Utc::now();

    let duplicate = leave_policy::Entity::find()
        .filter(leave_policy::Column::TenantId.eq(tenant_id))
        .filter(leave_policy::Column::LeaveTypeId.eq(lt.id))
        .all(db)
        .await
        .map_err(KabiPayError::from)?
        .into_iter()
        .any(|policy| id.map(|pid| pid != policy.id).unwrap_or(true));
    if duplicate {
        return Err(KabiPayError::Validation(
            "only one leave policy is allowed per leave type".into(),
        ));
    }

    if let Some(pid) = id {
        let row = leave_policy::Entity::find_by_id(pid)
            .filter(leave_policy::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "leave_policy",
                id: pid.to_string(),
            })?;
        let mut am: leave_policy::ActiveModel = row.into();
        am.leave_type_id = Set(lt.id);
        am.applicable_to = Set(applicable_to);
        am.annual_entitlement = Set(annual_entitlement);
        am.accrual_frequency = Set(accrual_frequency);
        am.accrual_days = Set(accrual_days);
        am.max_consecutive_days = Set(max_consecutive_days);
        am.min_notice_days = Set(min_notice_days);
        am.updated_at = Set(now);
        return Ok(am.update(db).await.map_err(KabiPayError::from)?);
    }

    let new_id = Uuid::new_v4();
    let am = leave_policy::ActiveModel {
        id: Set(new_id),
        tenant_id: Set(tenant_id),
        leave_type_id: Set(lt.id),
        applicable_to: Set(applicable_to),
        annual_entitlement: Set(annual_entitlement),
        accrual_frequency: Set(accrual_frequency),
        accrual_days: Set(accrual_days),
        max_consecutive_days: Set(max_consecutive_days),
        min_notice_days: Set(min_notice_days),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    leave_policy::Entity::find_by_id(new_id)
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("inserted leave_policy not found".into()))
}

pub async fn delete_leave_policy(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    policy_id: Uuid,
) -> KabiPayResult<bool> {
    let r = leave_policy::Entity::delete_many()
        .filter(leave_policy::Column::TenantId.eq(tenant_id))
        .filter(leave_policy::Column::Id.eq(policy_id))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(r.rows_affected > 0)
}

/// Effective entitlement from a policy row as of today. Fixed annual policies use
/// `annual_entitlement`; monthly policies use earned months in the current year.
pub fn entitled_days_from_policy(pol: &leave_policy::Model) -> Option<Decimal> {
    let as_of = Utc::now().date_naive();
    entitled_days_from_policy_as_of(pol, None, as_of.year(), as_of)
}

fn first_day_of_month(year: i32, month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, 1).expect("valid first day for a calendar month")
}

fn first_monthly_accrual_date(joining_date: NaiveDate) -> NaiveDate {
    if joining_date.day() == 1 {
        return joining_date;
    }
    if joining_date.month() == 12 {
        first_day_of_month(joining_date.year() + 1, 1)
    } else {
        first_day_of_month(joining_date.year(), joining_date.month() + 1)
    }
}

fn monthly_accrual_months(year: i32, joining_date: Option<NaiveDate>, as_of: NaiveDate) -> u32 {
    let year_start = first_day_of_month(year, 1);
    let year_end = NaiveDate::from_ymd_opt(year, 12, 31).expect("valid last day for December");
    if as_of < year_start {
        return 0;
    }
    let effective_as_of = if as_of > year_end { year_end } else { as_of };
    let employee_start = joining_date.map(first_monthly_accrual_date).unwrap_or(year_start);
    let start = if employee_start > year_start {
        employee_start
    } else {
        year_start
    };
    if start > effective_as_of {
        return 0;
    }
    ((effective_as_of.year() - start.year()) as u32 * 12)
        + effective_as_of.month()
        - start.month()
        + 1
}

pub fn entitled_days_from_policy_as_of(
    pol: &leave_policy::Model,
    joining_date: Option<NaiveDate>,
    year: i32,
    as_of: NaiveDate,
) -> Option<Decimal> {
    if let Some(a) = pol.annual_entitlement {
        return Some(Decimal::from(a));
    }
    let freq = pol
        .accrual_frequency
        .as_deref()
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_default();
    if freq == "MONTHLY" {
        let per = pol.accrual_days.unwrap_or(Decimal::ZERO);
        let months = monthly_accrual_months(year, joining_date, as_of);
        return Some(per * Decimal::from(months));
    }
    None
}

/// For every active employee and each distinct leave type policy (first policy row wins per type),
/// upsert `leave_balance` for `year` so `entitled_days` matches fixed annual entitlement or
/// monthly earned entitlement as of today.
/// Existing **used** / **pending** / **carried_forward** values are preserved; `balance_days` is recomputed.
/// Skips policy rows whose `applicable_to` is set to anything other than ALL / * / empty.
pub async fn provision_leave_balances_from_policies(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    year: i32,
) -> KabiPayResult<u32> {
    let policies = list_leave_policies(db, tenant_id, 500).await?;
    let mut seen_types = HashSet::new();
    let mut unique_policies: Vec<leave_policy::Model> = Vec::new();
    for p in policies {
        if seen_types.insert(p.leave_type_id) {
            unique_policies.push(p);
        }
    }

    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut touched: u32 = 0;
    let now = Utc::now();

    for emp in &employees {
        for pol in &unique_policies {
            if let Some(ref app) = pol.applicable_to {
                let t = app.trim().to_ascii_uppercase();
                if !t.is_empty() && t != "ALL" && t != "*" {
                    continue;
                }
            }
            let as_of = Utc::now().date_naive();
            let Some(target_entitled) =
                entitled_days_from_policy_as_of(pol, Some(emp.date_of_joining), year, as_of)
            else {
                continue;
            };
            if target_entitled <= Decimal::ZERO {
                continue;
            }

            let existing = leave_balance::Entity::find()
                .filter(leave_balance::Column::TenantId.eq(tenant_id))
                .filter(leave_balance::Column::EmployeeId.eq(emp.id))
                .filter(leave_balance::Column::LeaveTypeId.eq(pol.leave_type_id))
                .filter(leave_balance::Column::Year.eq(year))
                .one(db)
                .await
                .map_err(KabiPayError::from)?;

            if let Some(row) = existing {
                let balance_days = balance_days_from_components(
                    target_entitled,
                    row.carried_forward_days,
                    row.used_days,
                    row.pending_days,
                );
                let mut am: leave_balance::ActiveModel = row.into();
                am.entitled_days = Set(target_entitled);
                am.balance_days = Set(balance_days);
                am.updated_at = Set(now);
                am.update(db).await.map_err(KabiPayError::from)?;
            } else {
                let balance_days = compute_balance_days(
                    target_entitled,
                    Decimal::ZERO,
                    Decimal::ZERO,
                    Decimal::ZERO,
                )?;
                let new_id = Uuid::new_v4();
                let am = leave_balance::ActiveModel {
                    id: Set(new_id),
                    tenant_id: Set(tenant_id),
                    employee_id: Set(emp.id),
                    leave_type_id: Set(pol.leave_type_id),
                    year: Set(year),
                    entitled_days: Set(target_entitled),
                    used_days: Set(Decimal::ZERO),
                    pending_days: Set(Decimal::ZERO),
                    carried_forward_days: Set(Decimal::ZERO),
                    balance_days: Set(balance_days),
                    created_at: Set(now),
                    updated_at: Set(now),
                };
                am.insert(db).await.map_err(KabiPayError::from)?;
            }
            touched += 1;
        }
    }

    Ok(touched)
}

pub async fn upsert_leave_balance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
    year: i32,
    entitled_days: Decimal,
    used_days: Decimal,
    pending_days: Decimal,
    carried_forward_days: Decimal,
) -> KabiPayResult<leave_balance::Model> {
    employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;

    leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: leave_type_id.to_string(),
        })?;

    let balance_days = compute_balance_days(
        entitled_days,
        carried_forward_days,
        used_days,
        pending_days,
    )?;

    let now = Utc::now();

    let existing = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    if let Some(row) = existing {
        let mut am: leave_balance::ActiveModel = row.into();
        am.entitled_days = Set(entitled_days);
        am.used_days = Set(used_days);
        am.pending_days = Set(pending_days);
        am.carried_forward_days = Set(carried_forward_days);
        am.balance_days = Set(balance_days);
        am.updated_at = Set(now);
        return Ok(am.update(db).await.map_err(KabiPayError::from)?);
    }

    let new_id = Uuid::new_v4();
    let am = leave_balance::ActiveModel {
        id: Set(new_id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        leave_type_id: Set(leave_type_id),
        year: Set(year),
        entitled_days: Set(entitled_days),
        used_days: Set(used_days),
        pending_days: Set(pending_days),
        carried_forward_days: Set(carried_forward_days),
        balance_days: Set(balance_days),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    leave_balance::Entity::find_by_id(new_id)
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("inserted leave_balance not found".into()))
}

pub async fn adjust_leave_balance_entitlement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
    year: i32,
    entitled_delta: Decimal,
    also_credit_balance: bool,
) -> KabiPayResult<leave_balance::Model> {
    let row = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_balance",
            id: format!("{employee_id}-{leave_type_id}-{year}"),
        })?;

    let now = Utc::now();
    let entitled = row.entitled_days + entitled_delta;
    if entitled < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "entitled_days cannot go negative".into(),
        ));
    }
    let balance = if also_credit_balance {
        let b = row.balance_days + entitled_delta;
        if b < Decimal::ZERO {
            return Err(KabiPayError::Validation(
                "balance_days cannot go negative after adjustment".into(),
            ));
        }
        b
    } else {
        compute_balance_days(
            entitled,
            row.carried_forward_days,
            row.used_days,
            row.pending_days,
        )?
    };

    let mut am: leave_balance::ActiveModel = row.into();
    am.entitled_days = Set(entitled);
    am.balance_days = Set(balance);
    am.updated_at = Set(now);
    Ok(am.update(db).await.map_err(KabiPayError::from)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monthly_policy(days: Decimal) -> leave_policy::Model {
        let now = Utc::now();
        leave_policy::Model {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            leave_type_id: Uuid::nil(),
            applicable_to: None,
            annual_entitlement: None,
            accrual_frequency: Some("MONTHLY".into()),
            accrual_days: Some(days),
            max_consecutive_days: None,
            min_notice_days: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn monthly_policy_credits_only_elapsed_eligible_months() {
        let policy = monthly_policy(Decimal::new(125, 2));
        let joining = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        let as_of = NaiveDate::from_ymd_opt(2026, 8, 16).unwrap();

        assert_eq!(
            entitled_days_from_policy_as_of(&policy, Some(joining), 2026, as_of),
            Some(Decimal::new(1000, 2))
        );
    }

    #[test]
    fn monthly_policy_skips_partial_joining_month() {
        let policy = monthly_policy(Decimal::new(125, 2));
        let joining = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let as_of = NaiveDate::from_ymd_opt(2026, 8, 16).unwrap();

        assert_eq!(
            entitled_days_from_policy_as_of(&policy, Some(joining), 2026, as_of),
            Some(Decimal::new(625, 2))
        );
    }
}
