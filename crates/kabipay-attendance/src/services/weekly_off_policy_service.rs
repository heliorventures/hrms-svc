use chrono::{NaiveDate, Utc};
use kabipay_common::{
    tenant_business_clock::TenantBusinessClock,
    working_calendar::{
        audit_calendar, is_weekly_off, lock_calendar, validate_rule, WeeklyOffRule,
    },
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::{
    d0006_org_hierarchy::location,
    d0007_employee_core::employee,
    d0097_location_working_calendar::{
        employee_location_assignment as assignment, weekly_off_policy_version as policy,
        working_calendar_profile as profile,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

pub struct PolicyState {
    pub profile: Option<profile::Model>,
    pub versions: Vec<policy::Model>,
}
pub struct ScheduleCommand {
    pub location_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub inherits_default: bool,
    pub rule: WeeklyOffRule,
    pub expected_revision: i64,
}

pub fn preview(rule: &WeeklyOffRule, month: u32, year: i32) -> KabiPayResult<Vec<NaiveDate>> {
    validate_rule(rule)?;
    if !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "preview year must be between 1900 and 2200".into(),
        ));
    }
    let mut date = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| KabiPayError::Validation("invalid preview month".into()))?;
    let mut dates = Vec::new();
    loop {
        if is_weekly_off(date, rule) {
            dates.push(date);
        }
        let next = date
            .succ_opt()
            .ok_or_else(|| KabiPayError::Validation("preview date out of range".into()))?;
        if chrono::Datelike::month(&next) != month {
            break;
        }
        date = next;
    }
    Ok(dates)
}

async fn validate_location<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Option<Uuid>,
) -> KabiPayResult<()> {
    if let Some(id) = id {
        if location::Entity::find_by_id(id)
            .filter(location::Column::TenantId.eq(tenant))
            .filter(location::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .is_none()
        {
            return Err(KabiPayError::Validation(
                "select an active location in this company".into(),
            ));
        }
    }
    Ok(())
}

pub async fn read<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    location_id: Option<Uuid>,
) -> KabiPayResult<PolicyState> {
    validate_location(db, tenant, location_id).await?;
    let profile = profile::Entity::find_by_id(tenant).one(db).await?;
    let mut query = policy::Entity::find()
        .filter(policy::Column::TenantId.eq(tenant))
        .filter(policy::Column::SupersededAt.is_null());
    query = if let Some(id) = location_id {
        query.filter(policy::Column::LocationId.eq(id))
    } else {
        query.filter(policy::Column::LocationId.is_null())
    };
    Ok(PolicyState {
        profile,
        versions: query
            .order_by_asc(policy::Column::EffectiveFrom)
            .all(db)
            .await?,
    })
}

async fn insert_policy<C: ConnectionTrait>(
    txn: &C,
    tenant: Uuid,
    actor: Uuid,
    command: &ScheduleCommand,
) -> KabiPayResult<policy::Model> {
    let mut rule = command.rule.clone();
    rule.fixed_weekdays.sort_unstable();
    rule.saturday_ordinals.sort_unstable();
    Ok(policy::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant),
        location_id: Set(command.location_id),
        effective_from: Set(command.effective_from),
        inherits_default: Set(command.inherits_default),
        fixed_weekdays: Set(serde_json::json!(rule.fixed_weekdays)),
        saturday_ordinals: Set(serde_json::json!(rule.saturday_ordinals)),
        created_by: Set(actor),
        created_at: Set(Utc::now()),
        superseded_at: Set(None),
    }
    .insert(txn)
    .await?)
}

pub async fn activate(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    date: NaiveDate,
    clock: TenantBusinessClock,
) -> KabiPayResult<PolicyState> {
    if date != clock.now_date() {
        return Err(KabiPayError::Validation(
            "activate on today's company business date".into(),
        ));
    }
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant).await?;
    if let Some(existing) = profile::Entity::find_by_id(tenant)
        .lock_exclusive()
        .one(&txn)
        .await?
    {
        if existing.activation_date != date {
            return Err(KabiPayError::Validation(
                "working calendar is already activated on a different date".into(),
            ));
        }
        let state = read(&txn, tenant, None).await?;
        txn.commit().await?;
        return Ok(state);
    }
    let now = Utc::now();
    profile::ActiveModel {
        tenant_id: Set(tenant),
        activation_date: Set(date),
        revision: Set(1),
        activated_by: Set(actor),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    insert_policy(
        &txn,
        tenant,
        actor,
        &ScheduleCommand {
            location_id: None,
            effective_from: date,
            inherits_default: false,
            rule: WeeklyOffRule {
                fixed_weekdays: vec![6, 7],
                saturday_ordinals: vec![],
            },
            expected_revision: 0,
        },
    )
    .await?;
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::LocationId.is_not_null())
        .all(&txn)
        .await?;
    for employee in employees {
        let existing = assignment::Entity::find()
            .filter(assignment::Column::TenantId.eq(tenant))
            .filter(assignment::Column::EmployeeId.eq(employee.id))
            .order_by_desc(assignment::Column::EffectiveFrom)
            .one(&txn)
            .await?;
        if existing
            .as_ref()
            .is_some_and(|row| row.effective_from == date)
        {
            continue;
        }
        assignment::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant),
            employee_id: Set(employee.id),
            location_id: Set(employee.location_id),
            effective_from: Set(date),
            revision: Set(existing.map_or(1, |row| row.revision + 1)),
            changed_by: Set(actor),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    audit_calendar(
        &txn,
        tenant,
        actor,
        "working_calendar",
        tenant,
        "ACTIVATE",
        None,
        Some(serde_json::json!({"activationDate":date,"fixedWeekdays":[6,7]})),
    )
    .await?;
    let state = read(&txn, tenant, None).await?;
    txn.commit().await?;
    Ok(state)
}

pub async fn schedule(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    command: ScheduleCommand,
    clock: TenantBusinessClock,
) -> KabiPayResult<PolicyState> {
    validate_rule(&command.rule)?;
    if command.effective_from < clock.now_date() {
        return Err(KabiPayError::Validation(
            "weekly-off policy cannot start in the past".into(),
        ));
    }
    if command.inherits_default
        && (command.location_id.is_none()
            || !command.rule.fixed_weekdays.is_empty()
            || !command.rule.saturday_ordinals.is_empty())
    {
        return Err(KabiPayError::Validation(
            "inheritance is for a location and cannot include an override rule".into(),
        ));
    }
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant).await?;
    validate_location(&txn, tenant, command.location_id).await?;
    let profile = profile::Entity::find_by_id(tenant)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("activate the company working calendar first".into())
        })?;
    if profile.revision != command.expected_revision {
        return Err(KabiPayError::Validation(
            "working calendar changed; reload and try again".into(),
        ));
    }
    let existing = read(&txn, tenant, command.location_id)
        .await?
        .versions
        .into_iter()
        .find(|row| row.effective_from == command.effective_from);
    let before = existing.as_ref().map(|r| serde_json::json!({"id":r.id,"fixedWeekdays":r.fixed_weekdays,"saturdayOrdinals":r.saturday_ordinals,"inheritsDefault":r.inherits_default}));
    if let Some(row) = existing {
        let mut active = row.into_active_model();
        active.superseded_at = Set(Some(Utc::now()));
        active.update(&txn).await?;
    }
    let row = insert_policy(&txn, tenant, actor, &command).await?;
    let revision = profile.revision + 1;
    let mut active = profile.into_active_model();
    active.revision = Set(revision);
    active.updated_at = Set(Utc::now());
    active.update(&txn).await?;
    audit_calendar(&txn,tenant,actor,"weekly_off_policy",row.id,"SCHEDULE",before,Some(serde_json::json!({"locationId":row.location_id,"effectiveFrom":row.effective_from,"fixedWeekdays":row.fixed_weekdays,"saturdayOrdinals":row.saturday_ordinals,"inheritsDefault":row.inherits_default,"revision":revision}))).await?;
    let state = read(&txn, tenant, command.location_id).await?;
    txn.commit().await?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weekly_off_policy_preview_uses_shared_selected_saturday_rules() {
        let dates = preview(
            &WeeklyOffRule {
                fixed_weekdays: vec![7],
                saturday_ordinals: vec![2, 4],
            },
            10,
            2026,
        )
        .unwrap();
        assert!(dates.contains(&"2026-10-10".parse().unwrap()));
        assert!(dates.contains(&"2026-10-24".parse().unwrap()));
        assert!(!dates.contains(&"2026-10-31".parse().unwrap()));
        assert!(preview(&WeeklyOffRule::default(), 0, 2026).is_err());
        assert!(preview(&WeeklyOffRule::default(), 10, 100000).is_err());
    }
}
