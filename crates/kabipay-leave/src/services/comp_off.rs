#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use uuid::Uuid;

    fn policy(employee_id: Option<Uuid>, designation_id: Option<Uuid>) -> PolicyCandidate {
        PolicyCandidate { id: Uuid::new_v4(), employee_id, designation_id }
    }

    #[test]
    fn employee_policy_precedes_designation_and_tenant_policy() {
        let employee = Uuid::new_v4();
        let designation = Uuid::new_v4();
        let tenant = policy(None, None);
        let designation_policy = policy(None, Some(designation));
        let employee_policy = policy(Some(employee), None);
        let policies = vec![tenant, designation_policy, employee_policy.clone()];
        assert_eq!(select_policy(&policies, employee, Some(designation)).map(|p| p.id), Some(employee_policy.id));
    }

    #[test]
    fn independently_optional_limits_only_reject_exceeded_values() {
        let half = Decimal::new(5, 1);
        assert!(validate_earning_limits(half, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, None, None, None).is_ok());
        assert!(validate_earning_limits(half, half, Decimal::ZERO, Decimal::ZERO, Some(half), None, None).is_err());
        assert!(validate_earning_limits(half, Decimal::ZERO, half, Decimal::ZERO, None, Some(half), None).is_err());
        assert!(validate_earning_limits(half, Decimal::ZERO, Decimal::ZERO, half, None, None, Some(half)).is_err());
    }

    #[test]
    fn earliest_expiry_allocation_requires_each_leave_date_before_expiry() {
        let first = CreditAvailability { id: Uuid::new_v4(), expires_at: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), available: Decimal::ONE };
        let second = CreditAvailability { id: Uuid::new_v4(), expires_at: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(), available: Decimal::ONE };
        let allocations = plan_allocations(&[first.clone(), second.clone()], &[(NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), Decimal::ONE)]).unwrap();
        assert_eq!(allocations[0].credit_id, second.id);
        assert!(plan_allocations(&[first], &[(NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), Decimal::ONE)]).is_err());
    }

    #[test]
    fn approved_cancellation_requires_config_and_all_future_dates() {
        let today=NaiveDate::from_ymd_opt(2026,9,8).unwrap();
        assert!(approved_cancellation_allowed(true,today+Duration::days(1),today));
        assert!(!approved_cancellation_allowed(false,today+Duration::days(1),today));
        assert!(!approved_cancellation_allowed(true,today,today));
    }
    #[test]
    fn reserved_credit_remains_unused_for_balance_cap() {
        assert_eq!(unused_units(Decimal::ONE, Decimal::new(5, 1), Decimal::ZERO), Decimal::ONE);
    }

    #[test]
    fn released_credit_expiry_uses_the_supplied_tenant_business_date() {
        let expiry = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        assert_eq!(released_allocation_status(expiry, NaiveDate::from_ymd_opt(2026, 9, 8).unwrap()), "RELEASED");
        assert_eq!(released_allocation_status(expiry, expiry), "EXPIRED");
    }

    #[test]
    fn dedicated_comp_off_type_never_falls_back_to_annual_balance() {
        let leave_type_id=Uuid::new_v4();
        assert!(!validate_comp_off_policy_for_leave(false,None,leave_type_id).unwrap());
        assert!(validate_comp_off_policy_for_leave(true,None,leave_type_id).is_err());
        assert!(validate_comp_off_policy_for_leave(true,Some(Uuid::new_v4()),leave_type_id).is_err());
        assert!(validate_comp_off_policy_for_leave(true,Some(leave_type_id),leave_type_id).unwrap());
    }

    #[test]
    fn earning_periods_use_tenant_business_dates_at_the_india_new_year_boundary() {
        let jan_first = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        let dec_last = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert!(same_earning_month(jan_first, jan_first));
        assert!(!same_earning_month(dec_last, jan_first));
        assert!(!same_earning_year(dec_last, jan_first));
    }

    #[test]
    fn approved_future_leave_remains_outstanding_for_unused_cap() {
        let outstanding = unused_with_future_approved(Decimal::ONE, Decimal::ONE, Decimal::ONE);
        assert_eq!(outstanding, Decimal::ONE);
        assert!(validate_earning_limits(Decimal::ONE, Decimal::ZERO, Decimal::ZERO, outstanding, None, None, Some(Decimal::ONE)).is_err());
    }
}
use chrono::{Datelike, Duration, NaiveDate, Utc};
#[cfg(test)]
#[path = "comp_off_decision_tests.rs"]
mod comp_off_decision_tests;
use kabipay_common::client_data_scope::resolve_employee_scope_filter_with_connection;
use kabipay_common::context::{ClientViewerEmployee, ScopeType};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0011_leave::leave_type;
use kabipay_db_entities::tenant::d0078_comp_off::{comp_off_allocation, comp_off_claim, comp_off_credit, comp_off_policy};
use rust_decimal::Decimal;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};
use uuid::Uuid;

const PENDING: &str = "PENDING";
const APPROVED: &str = "APPROVED";
const REJECTED: &str = "REJECTED";
const CANCELLED: &str = "CANCELLED";

#[derive(Clone, Debug)]
pub struct PolicyCandidate { pub id: Uuid, pub employee_id: Option<Uuid>, pub designation_id: Option<Uuid> }

pub fn select_policy(policies: &[PolicyCandidate], employee_id: Uuid, designation_id: Option<Uuid>) -> Option<&PolicyCandidate> {
    policies.iter().find(|p| p.employee_id == Some(employee_id))
        .or_else(|| designation_id.and_then(|id| policies.iter().find(|p| p.employee_id.is_none() && p.designation_id == Some(id))))
        .or_else(|| policies.iter().find(|p| p.employee_id.is_none() && p.designation_id.is_none()))
}

pub fn validate_earning_limits(units: Decimal, month: Decimal, year: Decimal, unused: Decimal, monthly: Option<Decimal>, yearly: Option<Decimal>, max_unused: Option<Decimal>) -> KabiPayResult<()> {
    for (limit, current, label) in [(monthly, month, "monthly earning"), (yearly, year, "yearly earning"), (max_unused, unused, "maximum unused balance")] {
        if limit.is_some_and(|cap| current + units > cap) { return Err(KabiPayError::BusinessRule { code: "COMP_OFF_LIMIT_EXCEEDED", message: format!("Comp-off {label} limit would be exceeded.") }); }
    }
    Ok(())
}

pub fn approved_cancellation_allowed(configured: bool, first_leave_date: NaiveDate, today: NaiveDate) -> bool { configured && first_leave_date > today }
pub fn unused_units(earned: Decimal, _reserved: Decimal, used: Decimal) -> Decimal { earned - used }
pub fn unused_with_future_approved(earned: Decimal, used: Decimal, future_used: Decimal) -> Decimal { earned - used + future_used }
pub fn same_earning_month(approval_date: NaiveDate, today: NaiveDate) -> bool { approval_date.year() == today.year() && approval_date.month() == today.month() }
pub fn same_earning_year(approval_date: NaiveDate, today: NaiveDate) -> bool { approval_date.year() == today.year() }
pub fn released_allocation_status(expires_at: NaiveDate, business_date: NaiveDate) -> &'static str { if expires_at <= business_date { "EXPIRED" } else { "RELEASED" } }
pub fn validate_comp_off_policy_for_leave(is_dedicated_type:bool,policy_leave_type_id:Option<Uuid>,leave_type_id:Uuid)->KabiPayResult<bool>{if !is_dedicated_type{return Ok(false);}match policy_leave_type_id{Some(policy_type) if policy_type==leave_type_id=>Ok(true),Some(_)=>Err(KabiPayError::BusinessRule{code:"COMP_OFF_POLICY_INVALID",message:"The enabled comp-off policy is not linked to the dedicated COMP_OFF leave type.".into()}),None=>Err(KabiPayError::BusinessRule{code:"COMP_OFF_NOT_CONFIGURED",message:"Comp-off leave is unavailable until HR configures and enables a policy for you.".into()})}}

#[derive(Clone, Debug)]
pub struct CreditAvailability { pub id: Uuid, pub expires_at: NaiveDate, pub available: Decimal }
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedAllocation { pub credit_id: Uuid, pub leave_date: NaiveDate, pub units: Decimal }

pub fn plan_allocations(credits: &[CreditAvailability], dates: &[(NaiveDate, Decimal)]) -> KabiPayResult<Vec<PlannedAllocation>> {
    let mut credits = credits.to_vec();
    credits.sort_by_key(|credit| credit.expires_at);
    let mut output = Vec::new();
    for (leave_date, required) in dates {
        let mut remaining = *required;
        for credit in credits.iter_mut().filter(|credit| *leave_date < credit.expires_at && credit.available > Decimal::ZERO) {
            let units = remaining.min(credit.available);
            if units > Decimal::ZERO { output.push(PlannedAllocation { credit_id: credit.id, leave_date: *leave_date, units }); credit.available -= units; remaining -= units; }
            if remaining == Decimal::ZERO { break; }
        }
        if remaining > Decimal::ZERO { return Err(KabiPayError::BusinessRule { code: "COMP_OFF_BALANCE_UNAVAILABLE", message: format!("No unexpired comp-off credit covers leave date {leave_date}.") }); }
    }
    Ok(output)
}

pub async fn resolved_policy<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, employee_id: Uuid) -> KabiPayResult<Option<comp_off_policy::Model>> {
    let employee = employee::Entity::find_by_id(employee_id).filter(employee::Column::TenantId.eq(tenant_id)).filter(employee::Column::IsDeleted.eq(false)).one(db).await?.ok_or_else(|| KabiPayError::NotFound { entity: "employee", id: employee_id.to_string() })?;
    let rows = comp_off_policy::Entity::find().filter(comp_off_policy::Column::TenantId.eq(tenant_id)).all(db).await?;
    let candidates: Vec<_> = rows.iter().map(|p| PolicyCandidate { id: p.id, employee_id: p.employee_id, designation_id: p.designation_id }).collect();
    Ok(select_policy(&candidates, employee_id, employee.designation_id).and_then(|selected| rows.into_iter().find(|p| p.id == selected.id)).filter(|p| p.enabled))
}

pub async fn list_policies(db: &DatabaseConnection, tenant_id: Uuid) -> KabiPayResult<Vec<comp_off_policy::Model>> {
    Ok(comp_off_policy::Entity::find().filter(comp_off_policy::Column::TenantId.eq(tenant_id)).order_by_asc(comp_off_policy::Column::EmployeeId).order_by_asc(comp_off_policy::Column::DesignationId).all(db).await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_policy(db: &DatabaseConnection, tenant_id: Uuid, id: Option<Uuid>, designation_id: Option<Uuid>, employee_id: Option<Uuid>, enabled: bool, validity_days: i32, claim_deadline_days: i32, monthly: Option<Decimal>, yearly: Option<Decimal>, max_unused: Option<Decimal>, allow_cancel: bool) -> KabiPayResult<comp_off_policy::Model> {
    if validity_days <= 0 || claim_deadline_days < 0 || [monthly, yearly, max_unused].into_iter().flatten().any(|v| v <= Decimal::ZERO) { return Err(KabiPayError::Validation("Validity must be positive, claim deadline non-negative, and configured limits positive.".into())); }
    if designation_id.is_some() && employee_id.is_some() { return Err(KabiPayError::Validation("Choose either designation or employee scope, not both.".into())); }
    let leave_type_id=leave_type::Entity::find().filter(leave_type::Column::TenantId.eq(tenant_id)).filter(leave_type::Column::Code.eq("COMP_OFF")).filter(leave_type::Column::IsPaid.eq(true)).filter(leave_type::Column::HalfDayAllowed.eq(true)).filter(leave_type::Column::IsDeleted.eq(false)).one(db).await?.ok_or_else(||KabiPayError::BusinessRule{code:"COMP_OFF_LEAVE_TYPE_MISSING",message:"The dedicated paid COMP_OFF leave type is missing; apply migration 0078 before configuring comp-off.".into()})?.id;
    let now = Utc::now();
    let created_at = if let Some(policy_id)=id { comp_off_policy::Entity::find_by_id(policy_id).filter(comp_off_policy::Column::TenantId.eq(tenant_id)).one(db).await?.ok_or_else(||KabiPayError::NotFound{entity:"comp_off_policy",id:policy_id.to_string()})?.created_at } else { now };
    let model = comp_off_policy::ActiveModel { id: Set(id.unwrap_or_else(Uuid::new_v4)), tenant_id: Set(tenant_id), designation_id: Set(designation_id), employee_id: Set(employee_id), leave_type_id: Set(leave_type_id), enabled: Set(enabled), validity_days: Set(validity_days), claim_deadline_days: Set(claim_deadline_days), monthly_earning_limit: Set(monthly), yearly_earning_limit: Set(yearly), max_unused_balance: Set(max_unused), allow_approved_leave_cancellation: Set(allow_cancel), created_at: Set(created_at), updated_at: Set(now) };
    Ok(if id.is_some() { model.update(db).await? } else { model.insert(db).await? })
}

pub async fn submit_claim(db: &DatabaseConnection, tenant_id: Uuid, employee_id: Uuid, today: NaiveDate, worked_date: NaiveDate, units: Decimal, reason: Option<String>) -> KabiPayResult<comp_off_claim::Model> {
    if !matches!(units, value if value == Decimal::ONE || value == Decimal::new(5, 1)) { return Err(KabiPayError::Validation("Comp-off claim must be 0.5 or 1 day.".into())); }
    let txn = db.begin().await?;
    lock_employee(&txn, tenant_id, employee_id).await?;
    let policy = resolved_policy(&txn, tenant_id, employee_id).await?.ok_or_else(|| KabiPayError::BusinessRule { code: "COMP_OFF_NOT_CONFIGURED", message: "Comp-off is unavailable until HR configures and enables a policy.".into() })?;
    if worked_date > today || today.signed_duration_since(worked_date).num_days() > i64::from(policy.claim_deadline_days) { return Err(KabiPayError::BusinessRule { code: "COMP_OFF_CLAIM_DEADLINE", message: "Worked date is outside the configured claim window.".into() }); }
    let existing = comp_off_claim::Entity::find().filter(comp_off_claim::Column::TenantId.eq(tenant_id)).filter(comp_off_claim::Column::EmployeeId.eq(employee_id)).filter(comp_off_claim::Column::WorkedDate.eq(worked_date)).filter(comp_off_claim::Column::Status.is_in([PENDING, APPROVED])).all(&txn).await?;
    if existing.iter().map(|r| r.units).sum::<Decimal>() + units > Decimal::ONE { return Err(KabiPayError::BusinessRule { code: "COMP_OFF_WORKED_DATE_LIMIT", message: "Active claims for this worked date cannot exceed one day.".into() }); }
    let now = Utc::now();
    let row = comp_off_claim::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), employee_id: Set(employee_id), worked_date: Set(worked_date), units: Set(units), status: Set(PENDING.into()), reason: Set(reason.filter(|v| !v.trim().is_empty())), rejection_reason: Set(None), policy_id: Set(policy.id), policy_validity_days: Set(policy.validity_days), policy_claim_deadline_days: Set(policy.claim_deadline_days), policy_monthly_earning_limit: Set(policy.monthly_earning_limit), policy_yearly_earning_limit: Set(policy.yearly_earning_limit), policy_max_unused_balance: Set(policy.max_unused_balance), approved_by: Set(None), approved_at: Set(None), created_at: Set(now), updated_at: Set(now) }.insert(&txn).await?;
    txn.commit().await?; Ok(row)
}

pub async fn decide_claim(db: &DatabaseConnection, tenant_id: Uuid, claim_id: Uuid, actor_user_id: Uuid, actor_employee_id: Uuid, scope: ScopeType, today: NaiveDate, approve: bool, reason: Option<String>) -> KabiPayResult<comp_off_claim::Model> {
    let txn = db.begin().await?;
    let candidate = comp_off_claim::Entity::find_by_id(claim_id).filter(comp_off_claim::Column::TenantId.eq(tenant_id)).filter(comp_off_claim::Column::Status.eq(PENDING)).one(&txn).await?.ok_or_else(|| KabiPayError::BusinessRule { code: "COMP_OFF_DECISION_UNAVAILABLE", message: "Pending comp-off claim is unavailable.".into() })?;
    if candidate.employee_id == actor_employee_id { return Err(KabiPayError::Forbidden("Employees cannot decide their own comp-off claims.".into())); }
    let mut employee_ids=vec![actor_employee_id,candidate.employee_id];employee_ids.sort_unstable();employee_ids.dedup();let employees=employee::Entity::find().filter(employee::Column::TenantId.eq(tenant_id)).filter(employee::Column::Id.is_in(employee_ids)).filter(employee::Column::IsDeleted.eq(false)).order_by_asc(employee::Column::Id).lock_exclusive().all(&txn).await?;let actor=employees.iter().find(|row|row.id==actor_employee_id&&row.user_id==Some(actor_user_id)).ok_or_else(||KabiPayError::Forbidden("Comp-off decisions require an active linked employee.".into()))?;if !employees.iter().any(|row|row.id==candidate.employee_id){return Err(KabiPayError::BusinessRule{code:"COMP_OFF_DECISION_UNAVAILABLE",message:"Pending comp-off claim is unavailable.".into()});}
    let filter = resolve_employee_scope_filter_with_connection(&txn, tenant_id, scope, Some(ClientViewerEmployee { employee_id: actor.id, department_id: actor.department_id })).await?;
    if !filter.allows_employee(candidate.employee_id) { return Err(KabiPayError::Forbidden("Comp-off claim is outside your approval scope.".into())); }
    let row = comp_off_claim::Entity::find_by_id(claim_id).filter(comp_off_claim::Column::Status.eq(PENDING)).lock_exclusive().one(&txn).await?.ok_or_else(|| KabiPayError::BusinessRule { code: "COMP_OFF_DECISION_UNAVAILABLE", message: "Pending comp-off claim is unavailable.".into() })?;
    let now = Utc::now();
    let mut active: comp_off_claim::ActiveModel = row.clone().into();
    if approve {
        let credits = comp_off_credit::Entity::find().filter(comp_off_credit::Column::TenantId.eq(tenant_id)).filter(comp_off_credit::Column::EmployeeId.eq(row.employee_id)).all(&txn).await?;
        let monthly = credits.iter().filter(|c| same_earning_month(c.approval_business_date, today)).map(|c| c.earned_units).sum();
        let yearly = credits.iter().filter(|c| same_earning_year(c.approval_business_date, today)).map(|c| c.earned_units).sum();
        let active_credit_ids: Vec<_> = credits.iter().filter(|c| c.expires_at > today).map(|c| c.id).collect();
        let future_used: Decimal = if active_credit_ids.is_empty() { Decimal::ZERO } else {
            comp_off_allocation::Entity::find()
                .filter(comp_off_allocation::Column::TenantId.eq(tenant_id))
                .filter(comp_off_allocation::Column::CreditId.is_in(active_credit_ids))
                .filter(comp_off_allocation::Column::Status.eq("USED"))
                .filter(comp_off_allocation::Column::LeaveDate.gt(today))
                .all(&txn).await?.into_iter().map(|allocation| allocation.units).sum()
        };
        let active_earned = credits.iter().filter(|c| c.expires_at > today).map(|c| c.earned_units).sum();
        let active_used = credits.iter().filter(|c| c.expires_at > today).map(|c| c.used_units).sum();
        let unused = unused_with_future_approved(active_earned, active_used, future_used);
        validate_earning_limits(row.units, monthly, yearly, unused, row.policy_monthly_earning_limit, row.policy_yearly_earning_limit, row.policy_max_unused_balance)?;
        let expires_at = today.checked_add_signed(Duration::days(i64::from(row.policy_validity_days))).ok_or_else(|| KabiPayError::Validation("Configured comp-off validity exceeds the supported date range.".into()))?;
        comp_off_credit::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), employee_id: Set(row.employee_id), claim_id: Set(row.id), earned_units: Set(row.units), reserved_units: Set(Decimal::ZERO), used_units: Set(Decimal::ZERO), approved_at: Set(now), approval_business_date: Set(today), expires_at: Set(expires_at), created_at: Set(now), updated_at: Set(now) }.insert(&txn).await?;
        active.status = Set(APPROVED.into()); active.approved_by = Set(Some(actor_user_id)); active.approved_at = Set(Some(now)); active.rejection_reason = Set(None);
    } else { active.status = Set(REJECTED.into()); active.rejection_reason = Set(reason.filter(|v| !v.trim().is_empty())); }
    active.updated_at = Set(now); let output = active.update(&txn).await?; txn.commit().await?; Ok(output)
}

pub async fn cancel_claim(db: &DatabaseConnection, tenant_id: Uuid, claim_id: Uuid, employee_id: Uuid) -> KabiPayResult<comp_off_claim::Model> {
    let txn = db.begin().await?;
    lock_employee(&txn, tenant_id, employee_id).await?;
    let row = comp_off_claim::Entity::find_by_id(claim_id).filter(comp_off_claim::Column::TenantId.eq(tenant_id)).filter(comp_off_claim::Column::EmployeeId.eq(employee_id)).filter(comp_off_claim::Column::Status.eq(PENDING)).lock_exclusive().one(&txn).await?.ok_or_else(|| KabiPayError::BusinessRule { code: "COMP_OFF_CANCEL_UNAVAILABLE", message: "Only your pending comp-off claim can be cancelled.".into() })?;
    let mut active: comp_off_claim::ActiveModel = row.into(); active.status = Set(CANCELLED.into()); active.updated_at = Set(Utc::now()); let output = active.update(&txn).await?; txn.commit().await?; Ok(output)
}

pub async fn claim_target(db: &DatabaseConnection, tenant_id: Uuid, claim_id: Uuid) -> KabiPayResult<comp_off_claim::Model> {
    comp_off_claim::Entity::find_by_id(claim_id).filter(comp_off_claim::Column::TenantId.eq(tenant_id)).one(db).await?.ok_or_else(|| KabiPayError::NotFound { entity: "comp_off_claim", id: claim_id.to_string() })
}

pub async fn list_claims<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, employee_ids: Option<Vec<Uuid>>, exclude_employee_id: Option<Uuid>, status: Option<&str>, limit: u64, offset: u64) -> KabiPayResult<Vec<comp_off_claim::Model>> {
    let mut query = comp_off_claim::Entity::find().filter(comp_off_claim::Column::TenantId.eq(tenant_id));
    if let Some(ids) = employee_ids { if ids.is_empty() { return Ok(vec![]); } query = query.filter(comp_off_claim::Column::EmployeeId.is_in(ids)); }
    if let Some(employee_id)=exclude_employee_id { query=query.filter(comp_off_claim::Column::EmployeeId.ne(employee_id)); }
    if let Some(value)=status.map(str::trim).filter(|value|!value.is_empty()){if ![PENDING,APPROVED,REJECTED,CANCELLED].contains(&value){return Err(KabiPayError::Validation("status must be PENDING, APPROVED, REJECTED, or CANCELLED".into()));}query=query.filter(comp_off_claim::Column::Status.eq(value));}
    Ok(query.order_by_desc(comp_off_claim::Column::CreatedAt).offset(offset).limit(limit.clamp(1, 200)).all(db).await?)
}

pub async fn balance<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, employee_id: Uuid, today: NaiveDate) -> KabiPayResult<(Decimal, Decimal, Decimal, Decimal)> {
    let rows = comp_off_credit::Entity::find().filter(comp_off_credit::Column::TenantId.eq(tenant_id)).filter(comp_off_credit::Column::EmployeeId.eq(employee_id)).all(db).await?;
    let earned = rows.iter().map(|r| r.earned_units).sum(); let reserved = rows.iter().map(|r| r.reserved_units).sum(); let used = rows.iter().map(|r| r.used_units).sum(); let expired = rows.iter().filter(|r| r.expires_at <= today).map(|r| r.earned_units-r.reserved_units-r.used_units).sum(); Ok((earned, reserved, used, expired))
}

async fn lock_employee<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, employee_id: Uuid) -> KabiPayResult<()> { employee::Entity::find_by_id(employee_id).filter(employee::Column::TenantId.eq(tenant_id)).filter(employee::Column::IsDeleted.eq(false)).lock_exclusive().one(db).await?.ok_or_else(|| KabiPayError::NotFound { entity: "employee", id: employee_id.to_string() })?; Ok(()) }

pub async fn reserve_for_leave(txn: &DatabaseTransaction, tenant_id: Uuid, employee_id: Uuid, leave_request_id: Uuid, leave_type_id: Uuid, today: NaiveDate, dates: &[(NaiveDate, Decimal)]) -> KabiPayResult<bool> {
    let is_dedicated_type=leave_type::Entity::find_by_id(leave_type_id).filter(leave_type::Column::TenantId.eq(tenant_id)).filter(leave_type::Column::Code.eq("COMP_OFF")).filter(leave_type::Column::IsDeleted.eq(false)).one(txn).await?.is_some();
    if !is_dedicated_type { return Ok(false); }
    let policy=resolved_policy(txn,tenant_id,employee_id).await?;
    if !validate_comp_off_policy_for_leave(is_dedicated_type,policy.as_ref().map(|value|value.leave_type_id),leave_type_id)?{return Ok(false);}
    lock_employee(txn, tenant_id, employee_id).await?;
    let credits = comp_off_credit::Entity::find().filter(comp_off_credit::Column::TenantId.eq(tenant_id)).filter(comp_off_credit::Column::EmployeeId.eq(employee_id)).filter(comp_off_credit::Column::ExpiresAt.gt(today)).order_by_asc(comp_off_credit::Column::ExpiresAt).lock_exclusive().all(txn).await?;
    let available: Vec<_> = credits.iter().map(|c| CreditAvailability { id: c.id, expires_at: c.expires_at, available: c.earned_units-c.reserved_units-c.used_units }).collect(); let planned = plan_allocations(&available, dates)?; let now = Utc::now();
    let mut totals=std::collections::HashMap::<Uuid,Decimal>::new(); for allocation in &planned { *totals.entry(allocation.credit_id).or_default() += allocation.units; }
    for credit in &credits { if let Some(units)=totals.get(&credit.id) { let mut active: comp_off_credit::ActiveModel=credit.clone().into(); active.reserved_units=Set(credit.reserved_units+*units); active.updated_at=Set(now); active.update(txn).await?; } }
    for allocation in planned { comp_off_allocation::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), credit_id: Set(allocation.credit_id), leave_request_id: Set(leave_request_id), leave_date: Set(allocation.leave_date), units: Set(allocation.units), status: Set("RESERVED".into()), created_at: Set(now), updated_at: Set(now) }.insert(txn).await?; }
    Ok(true)
}

pub async fn finalize_leave_allocations<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, leave_request_id: Uuid, today: NaiveDate, approve: bool) -> KabiPayResult<()> {
    let rows = comp_off_allocation::Entity::find().filter(comp_off_allocation::Column::TenantId.eq(tenant_id)).filter(comp_off_allocation::Column::LeaveRequestId.eq(leave_request_id)).filter(comp_off_allocation::Column::Status.eq("RESERVED")).lock_exclusive().all(db).await?; let now=Utc::now();
    for row in rows { let credit=comp_off_credit::Entity::find_by_id(row.credit_id).lock_exclusive().one(db).await?.ok_or_else(|| KabiPayError::Internal("comp-off credit missing".into()))?; let mut ca: comp_off_credit::ActiveModel=credit.clone().into(); ca.reserved_units=Set(credit.reserved_units-row.units); if approve { ca.used_units=Set(credit.used_units+row.units); } ca.updated_at=Set(now); ca.update(db).await?; let mut aa: comp_off_allocation::ActiveModel=row.into(); aa.status=Set(if approve {"USED"} else {released_allocation_status(credit.expires_at,today)}.into()); aa.updated_at=Set(now); aa.update(db).await?; }
    Ok(())
}

pub async fn cancel_used_leave_allocations<C: ConnectionTrait + Sync>(db:&C,tenant_id:Uuid,leave_request_id:Uuid,today:NaiveDate)->KabiPayResult<()> { let rows=comp_off_allocation::Entity::find().filter(comp_off_allocation::Column::TenantId.eq(tenant_id)).filter(comp_off_allocation::Column::LeaveRequestId.eq(leave_request_id)).filter(comp_off_allocation::Column::Status.eq("USED")).lock_exclusive().all(db).await?; let now=Utc::now(); if rows.iter().any(|r|r.leave_date<=today){return Err(KabiPayError::BusinessRule{code:"COMP_OFF_LEAVE_ALREADY_STARTED",message:"Approved comp-off leave can only be cancelled before every leave date.".into()});} for row in rows {let credit=comp_off_credit::Entity::find_by_id(row.credit_id).lock_exclusive().one(db).await?.ok_or_else(||KabiPayError::Internal("comp-off credit missing".into()))?;let mut ca:comp_off_credit::ActiveModel=credit.clone().into();ca.used_units=Set(credit.used_units-row.units);ca.updated_at=Set(now);ca.update(db).await?;let mut aa:comp_off_allocation::ActiveModel=row.into();aa.status=Set(released_allocation_status(credit.expires_at,today).into());aa.updated_at=Set(now);aa.update(db).await?;}Ok(()) }
