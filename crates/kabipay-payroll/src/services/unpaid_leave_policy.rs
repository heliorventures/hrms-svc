use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0010_time_shift_roster::{holiday, holiday_calendar},
    d0011_leave::{leave_request, leave_type},
    d0012_payroll::salary_component,
    d0077_unpaid_leave_payroll::{payroll_unpaid_leave_policy as policy, payslip_unpaid_leave as snapshot, payroll_unpaid_leave_allocation as allocation},
};
use rust_decimal::Decimal;
use sea_orm::{sea_query::OnConflict, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, QueryOrder, Set};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use super::{unpaid_leave_allocation::allocate_dates, unpaid_leave_calculation::calculate_unpaid_leave};

pub const BEFORE: &str = "BEFORE_STATUTORY";
pub const AFTER: &str = "AFTER_STATUTORY";
pub const DEDUCTION_CODE: &str = "UNPAID_LEAVE";

pub fn ensure_manual_component(code: &str) -> KabiPayResult<()> {
    if code.eq_ignore_ascii_case(DEDUCTION_CODE) {
        return Err(KabiPayError::Validation("UNPAID_LEAVE is managed by payroll; remove it from manual salary structures and configure the unpaid leave policy instead".into()));
    }
    Ok(())
}

pub fn validate(enabled: bool, code: Option<&str>, divisor: Option<Decimal>, treatment: Option<&str>) -> KabiPayResult<()> {
    if code.is_some_and(|v| v.is_empty() || v.len() > 50) {
        return Err(KabiPayError::Validation("basic component code must contain 1 to 50 characters".into()));
    }
    if divisor.is_some_and(|v| v <= Decimal::ZERO || v >= Decimal::from(1_000_000) || v.scale() > 4) {
        return Err(KabiPayError::Validation("day divisor must be greater than zero, below 1000000, with up to four decimal places".into()));
    }
    if treatment.is_some_and(|v| v != BEFORE && v != AFTER) {
        return Err(KabiPayError::Validation("select before statutory or after statutory treatment".into()));
    }
    if enabled && (code.is_none() || divisor.is_none() || treatment.is_none()) {
        return Err(KabiPayError::Validation("basic component, day divisor and treatment are required to enable unpaid leave deduction".into()));
    }
    Ok(())
}

pub async fn find<C: ConnectionTrait + Sync>(db: &C, tenant: Uuid) -> KabiPayResult<Option<policy::Model>> {
    policy::Entity::find_by_id(tenant).one(db).await.map_err(KabiPayError::from)
}

pub async fn save<C: ConnectionTrait + Sync>(db: &C, tenant: Uuid, actor: Uuid, enabled: bool, code: Option<String>, divisor: Option<Decimal>, treatment: Option<String>) -> KabiPayResult<policy::Model> {
    let code = code.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    validate(enabled, code.as_deref(), divisor, treatment.as_deref())?;
    if enabled {
        let component = salary_component::Entity::find()
            .filter(salary_component::Column::TenantId.eq(tenant))
            .filter(salary_component::Column::Code.eq(code.clone().unwrap_or_default()))
            .filter(salary_component::Column::IsActive.eq(true))
            .filter(salary_component::Column::Type.eq("EARNING"))
            .one(db).await.map_err(KabiPayError::from)?;
        if component.is_none() { return Err(KabiPayError::Validation("select an active basic earning component belonging to this company".into())); }
    }
    let now = Utc::now();
    policy::Entity::insert(policy::ActiveModel {
        tenant_id: Set(tenant), enabled: Set(enabled), basic_component_code: Set(code), day_divisor: Set(divisor),
        treatment: Set(treatment), updated_by: Set(actor), created_at: Set(now), updated_at: Set(now),
    }).on_conflict(OnConflict::column(policy::Column::TenantId).update_columns([
        policy::Column::Enabled, policy::Column::BasicComponentCode, policy::Column::DayDivisor,
        policy::Column::Treatment, policy::Column::UpdatedBy, policy::Column::UpdatedAt,
    ]).to_owned()).exec(db).await.map_err(KabiPayError::from)?;
    find(db, tenant).await?.ok_or_else(|| KabiPayError::Validation("unpaid leave policy was not saved".into()))
}

pub struct Calculation {
    pub basic: Decimal,
    pub days: Decimal,
    pub amount: Decimal,
    pub source_days: serde_json::Value,
}

fn resolved_dates(saved: Option<&allocation::Model>, request: &leave_request::Model, sandwich: bool, holidays: &HashSet<NaiveDate>) -> KabiPayResult<Vec<(NaiveDate, Decimal)>> {
    if let Some(saved) = saved {
        if saved.from_date != request.from_date || saved.to_date != request.to_date || saved.approved_days != request.days_requested {
            return Err(KabiPayError::Validation(format!("unpaid leave request {} changed after payroll consumption; HR review required", request.id)));
        }
        serde_json::from_value(saved.date_units.clone()).map_err(|_| KabiPayError::Validation("stored unpaid leave allocation is invalid; HR review required".into()))
    } else {
        allocate_dates(request.from_date, request.to_date, request.is_half_day, sandwich, request.days_requested, holidays)
    }
}

pub async fn calculate<C: ConnectionTrait + Sync>(db: &C, tenant: Uuid, employee: Uuid, period: NaiveDate, policy: &policy::Model, basic: Decimal) -> KabiPayResult<Calculation> {
    validate(policy.enabled, policy.basic_component_code.as_deref(), policy.day_divisor, policy.treatment.as_deref())?;
    let end = if period.month() == 12 { NaiveDate::from_ymd_opt(period.year() + 1, 1, 1) } else { NaiveDate::from_ymd_opt(period.year(), period.month() + 1, 1) }
        .and_then(|v| v.pred_opt()).ok_or_else(|| KabiPayError::Validation("invalid payroll month".into()))?;
    let types = leave_type::Entity::find().filter(leave_type::Column::TenantId.eq(tenant))
        .filter(leave_type::Column::IsPaid.eq(false)).all(db).await.map_err(KabiPayError::from)?;
    let types: HashMap<_, _> = types.into_iter().map(|v| (v.id, v)).collect();
    let requests = if types.is_empty() { Vec::new() } else {
        leave_request::Entity::find().filter(leave_request::Column::TenantId.eq(tenant))
            .filter(leave_request::Column::EmployeeId.eq(employee))
            .filter(leave_request::Column::LeaveTypeId.is_in(types.keys().copied()))
            .filter(leave_request::Column::UsesCompOff.eq(false))
            .filter(leave_request::Column::Status.eq("APPROVED"))
            .filter(leave_request::Column::IsDeleted.eq(false))
            .filter(leave_request::Column::FromDate.lte(end))
            .filter(leave_request::Column::ToDate.gte(period))
            .order_by_asc(leave_request::Column::Id).lock_exclusive().all(db).await.map_err(KabiPayError::from)?
    };
    let mut holidays = HashSet::new();
    if !requests.is_empty() {
        let from = requests.iter().map(|r| r.from_date).min().unwrap_or(period);
        let to = requests.iter().map(|r| r.to_date).max().unwrap_or(end);
        let calendars = holiday_calendar::Entity::find().filter(holiday_calendar::Column::TenantId.eq(tenant)).all(db).await.map_err(KabiPayError::from)?;
        if !calendars.is_empty() {
            holidays = holiday::Entity::find().filter(holiday::Column::CalendarId.is_in(calendars.iter().map(|c| c.id)))
                .filter(holiday::Column::HolidayDate.between(from, to)).all(db).await.map_err(KabiPayError::from)?
                .into_iter().map(|h| h.holiday_date).collect();
        }
    }
    let mut days = Decimal::ZERO;
    let mut source = Vec::new();
    let mut per_date: HashMap<NaiveDate, Decimal> = HashMap::new();
    for request in requests {
        let leave_type = &types[&request.leave_type_id];
        // The request lock serializes first consumption across payroll months. Freeze
        // the WHOLE approved allocation so later calendar changes cannot charge it twice.
        let existing = allocation::Entity::find_by_id(request.id).filter(allocation::Column::TenantId.eq(tenant))
            .filter(allocation::Column::EmployeeId.eq(employee)).one(db).await.map_err(KabiPayError::from)?;
        let dates = resolved_dates(existing.as_ref(), &request, leave_type.sandwich_rule, &holidays)?;
        if existing.is_none() {
            allocation::ActiveModel {
                leave_request_id: Set(request.id), tenant_id: Set(tenant), employee_id: Set(employee),
                from_date: Set(request.from_date), to_date: Set(request.to_date), approved_days: Set(request.days_requested),
                date_units: Set(serde_json::to_value(&dates).map_err(|_| KabiPayError::Validation("cannot save unpaid leave allocation".into()))?), created_at: Set(Utc::now()),
            }.insert(db).await.map_err(KabiPayError::from)?;
        }
        for (date, units) in dates {
            if date < period || date > end { continue; }
            let total = per_date.entry(date).or_default();
            *total += units;
            if *total > Decimal::ONE { return Err(KabiPayError::Validation(format!("overlapping approved unpaid leave on {date} for employee {employee}; HR review required"))); }
            days += units;
            source.push(serde_json::json!({ "requestId": request.id, "date": date, "days": units.to_string() }));
        }
    }
    let amount = calculate_unpaid_leave(basic, policy.day_divisor.ok_or_else(|| KabiPayError::Validation("missing unpaid leave divisor".into()))?, days)?;
    if amount > basic { return Err(KabiPayError::Validation(format!("unpaid leave deduction exceeds basic salary for employee {employee}; review the configured divisor and leave dates"))); }
    Ok(Calculation { basic, days, amount, source_days: serde_json::Value::Array(source) })
}

pub async fn deduction_component<C: ConnectionTrait + Sync>(db: &C, tenant: Uuid) -> KabiPayResult<salary_component::Model> {
    let now = Utc::now();
    salary_component::Entity::insert(salary_component::ActiveModel {
        id: Set(Uuid::new_v4()), tenant_id: Set(tenant), name: Set("Unpaid leave".into()), code: Set(DEDUCTION_CODE.into()),
        r#type: Set("DEDUCTION".into()), is_taxable: Set(false), is_fixed: Set(false), is_active: Set(true), formula_expression: Set(None), created_at: Set(now), updated_at: Set(now),
    }).on_conflict(OnConflict::columns([salary_component::Column::TenantId, salary_component::Column::Code]).do_nothing().to_owned())
        .do_nothing().exec(db).await.map_err(KabiPayError::from)?;
    let row = salary_component::Entity::find().filter(salary_component::Column::TenantId.eq(tenant)).filter(salary_component::Column::Code.eq(DEDUCTION_CODE))
        .one(db).await.map_err(KabiPayError::from)?.ok_or_else(|| KabiPayError::Validation("unpaid leave component missing".into()))?;
    if !row.is_active || row.r#type != "DEDUCTION" { return Err(KabiPayError::Validation("UNPAID_LEAVE component must be an active deduction".into())); }
    Ok(row)
}

pub async fn record<C: ConnectionTrait + Sync>(db: &C, tenant: Uuid, payslip: Uuid, policy: &policy::Model, calc: Calculation) -> KabiPayResult<()> {
    snapshot::ActiveModel {
        id: Set(Uuid::new_v4()), tenant_id: Set(tenant), payslip_id: Set(payslip),
        basic_component_code: Set(policy.basic_component_code.clone().unwrap_or_default()),
        basic_amount: Set(calc.basic), day_divisor: Set(policy.day_divisor.unwrap_or_default()),
        unpaid_days: Set(calc.days), amount: Set(calc.amount), treatment: Set(policy.treatment.clone().unwrap_or_default()),
        source_days: Set(calc.source_days), created_at: Set(Utc::now()),
    }.insert(db).await.map_err(KabiPayError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn disabled_policy_requires_no_formula() { assert!(validate(false, None, None, None).is_ok()); }
    #[test] fn enabling_requires_explicit_complete_formula() {
        assert!(validate(true, Some("BASIC"), None, Some(BEFORE)).is_err());
        assert!(validate(true, Some("BASIC"), Some(30.into()), None).is_err());
        assert!(validate(true, None, Some(30.into()), Some(AFTER)).is_err());
        for treatment in [BEFORE, AFTER] { assert!(validate(true, Some("BASIC"), Some(30.into()), Some(treatment)).is_ok()); }
    }
    #[test] fn invalid_formula_is_rejected_even_when_disabled() {
        assert!(validate(false, None, Some(Decimal::ZERO), None).is_err());
        assert!(validate(false, None, None, Some("UNKNOWN")).is_err());
    }
    #[test] fn automatic_deduction_is_never_a_manual_salary_component() {
        assert!(ensure_manual_component("UNPAID_LEAVE").is_err());
        assert!(ensure_manual_component("unpaid_leave").is_err());
        assert!(ensure_manual_component("BASIC").is_ok());
    }
    #[test] fn consumed_spanning_leave_keeps_dates_when_calendar_moves() {
        let now = Utc::now();
        let request = leave_request::Model {
            id: Uuid::new_v4(), tenant_id: Uuid::new_v4(), employee_id: Uuid::new_v4(), leave_type_id: Uuid::new_v4(),
            from_date: "2026-09-30".parse().unwrap(), to_date: "2026-10-01".parse().unwrap(),
            days_requested: Decimal::ONE, is_half_day: false, half_day_session: None, status: "APPROVED".into(),
            reason: None, rejection_reason: None, supporting_document_reference: None, supporting_document_file_storage_id: None,
            approved_by: None, workflow_instance_id: None, uses_comp_off: false, applied_at: now, is_deleted: false,
            deleted_at: None, deleted_by: None, created_at: now, updated_at: now,
        };
        let dates = resolved_dates(None, &request, false, &HashSet::from([request.to_date])).unwrap();
        assert_eq!(dates, vec![(request.from_date, Decimal::ONE)]);
        let saved = allocation::Model {
            leave_request_id: request.id, tenant_id: request.tenant_id, employee_id: request.employee_id,
            from_date: request.from_date, to_date: request.to_date, approved_days: Decimal::ONE,
            date_units: serde_json::to_value(&dates).unwrap(), created_at: now,
        };
        let after_calendar_change = resolved_dates(Some(&saved), &request, false, &HashSet::from([request.from_date])).unwrap();
        assert_eq!(after_calendar_change, dates);
        assert!(after_calendar_change.iter().all(|(date, _)| date.month() != 10));
        let mut changed_request = request;
        changed_request.days_requested = Decimal::from(2);
        assert!(resolved_dates(Some(&saved), &changed_request, false, &HashSet::new()).is_err());
    }
}
