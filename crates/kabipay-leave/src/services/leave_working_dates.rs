use chrono::{Datelike, NaiveDate, Utc, Weekday};
use kabipay_common::{working_calendar::WorkingCalendarSnapshot, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0098_leave_working_dates::leave_working_date_snapshot as saved;
use rust_decimal::Decimal;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use std::collections::HashSet;
use uuid::Uuid;

pub fn build_leave_date_units(
    snapshot: &WorkingCalendarSnapshot,
    employee: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    half_day: bool,
    sandwich: bool,
    legacy_holidays: &HashSet<NaiveDate>,
) -> KabiPayResult<Vec<(NaiveDate, Decimal)>> {
    if from > to || (to - from).num_days() > 3660 || (half_day && from != to) {
        return Err(KabiPayError::Validation("invalid leave date range".into()));
    }
    let mut result = Vec::new();
    let mut date = from;
    loop {
        let day = snapshot.day(employee, date)?;
        let excluded = if day.activated {
            day.weekly_off || day.holiday
        } else {
            matches!(date.weekday(), Weekday::Sat | Weekday::Sun) || legacy_holidays.contains(&date)
        };
        if half_day || sandwich || !excluded {
            result.push((
                date,
                if half_day {
                    Decimal::new(5, 1)
                } else {
                    Decimal::ONE
                },
            ));
        }
        if date == to {
            break;
        }
        date = date
            .succ_opt()
            .ok_or_else(|| KabiPayError::Validation("leave date out of range".into()))?;
    }
    if result.is_empty() {
        return Err(KabiPayError::Validation(
            "no chargeable working days in this date range".into(),
        ));
    }
    Ok(result)
}

pub async fn save_leave_date_snapshot<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant: Uuid,
    request: Uuid,
    employee: Uuid,
    snapshot: &WorkingCalendarSnapshot,
    dates: &[(NaiveDate, Decimal)],
) -> KabiPayResult<()> {
    if snapshot
        .activation_date
        .is_none_or(|activation| snapshot.to_date < activation)
    {
        return Ok(());
    }
    let mut provenance = Vec::new();
    let mut date = snapshot.from_date;
    loop {
        provenance.push(serde_json::json!({"date":date,"calendar":snapshot.day(employee,date)?}));
        if date == snapshot.to_date {
            break;
        }
        date = date
            .succ_opt()
            .ok_or_else(|| KabiPayError::Validation("leave date out of range".into()))?;
    }
    saved::ActiveModel {
        leave_request_id: Set(request), tenant_id: Set(tenant), employee_id: Set(employee), from_date: Set(snapshot.from_date), to_date: Set(snapshot.to_date),
        requested_days: Set(dates.iter().map(|(_,units)|*units).sum()), date_units: Set(serde_json::json!(dates.iter().map(|(date,units)|serde_json::json!({"date":date,"units":units.to_string()})).collect::<Vec<_>>())),
        calendar_provenance: Set(serde_json::json!({"activationDate":snapshot.activation_date,"revision":snapshot.revision,"days":provenance})), created_at: Set(Utc::now()),
    }.insert(txn).await?;
    Ok(())
}

pub async fn load_leave_date_snapshot<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    request: Uuid,
) -> KabiPayResult<Option<saved::Model>> {
    Ok(saved::Entity::find_by_id(request)
        .filter(saved::Column::TenantId.eq(tenant))
        .one(db)
        .await?)
}

pub async fn validate_saved_request_dates<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    request: &kabipay_db_entities::tenant::d0011_leave::leave_request::Model,
) -> KabiPayResult<()> {
    if let Some(snapshot) = load_leave_date_snapshot(db, tenant, request.id).await? {
        if snapshot.employee_id != request.employee_id
            || snapshot.from_date != request.from_date
            || snapshot.to_date != request.to_date
            || snapshot.requested_days != request.days_requested
        {
            return Err(KabiPayError::Validation(
                "leave request no longer matches its saved working dates; HR review required"
                    .into(),
            ));
        }
        kabipay_common::working_calendar::decode_leave_units(
            snapshot.date_units,
            request.from_date,
            request.to_date,
            request.days_requested,
        )?;
    }
    Ok(())
}

pub async fn preview_leave_date_units(
    db: &sea_orm::DatabaseConnection,
    tenant: Uuid,
    employee: Uuid,
    leave_type_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    half_day: bool,
) -> KabiPayResult<Vec<(NaiveDate, Decimal)>> {
    use kabipay_db_entities::tenant::d0011_leave::leave_type;
    use sea_orm::TransactionTrait;
    if from > to || (to - from).num_days() > 3660 {
        return Err(KabiPayError::Validation("invalid leave date range".into()));
    }
    let txn = db.begin().await?;
    kabipay_common::working_calendar::lock_calendar(&txn, tenant).await?;
    let leave_type = leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(&txn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("select an active leave type in this company".into())
        })?;
    if half_day && !leave_type.half_day_allowed {
        return Err(KabiPayError::Validation(
            "this leave type does not allow half days".into(),
        ));
    }
    let calendar =
        kabipay_common::working_calendar::load_calendar(&txn, tenant, &[employee], from, to)
            .await?;
    let holidays =
        super::leave_service::tenant_holiday_dates_between(&txn, tenant, from, to).await?;
    let dates = build_leave_date_units(
        &calendar,
        employee,
        from,
        to,
        half_day,
        leave_type.sandwich_rule,
        &holidays,
    )?;
    txn.commit().await?;
    Ok(dates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kabipay_common::working_calendar::{PolicyVersion, WeeklyOffRule};
    #[test]
    fn leave_working_dates_span_activation_half_day_and_sandwich() {
        let employee = Uuid::new_v4();
        let from: NaiveDate = "2026-09-30".parse().unwrap();
        let to: NaiveDate = "2026-10-11".parse().unwrap();
        let snapshot = WorkingCalendarSnapshot {
            activation_date: Some("2026-10-01".parse().unwrap()),
            revision: Some(1),
            from_date: from,
            to_date: to,
            employees: [employee].into(),
            assignments: Default::default(),
            policies: [(
                None,
                vec![PolicyVersion {
                    id: Uuid::new_v4(),
                    effective_from: "2026-10-01".parse().unwrap(),
                    inherits_default: false,
                    rule: WeeklyOffRule {
                        fixed_weekdays: vec![7],
                        saturday_ordinals: vec![2, 4],
                    },
                }],
            )]
            .into(),
            holidays: Default::default(),
            roster_work: Default::default(),
        };
        let units =
            build_leave_date_units(&snapshot, employee, from, to, false, false, &HashSet::new())
                .unwrap();
        assert!(units
            .iter()
            .any(|(date, _)| *date == "2026-10-03".parse::<NaiveDate>().unwrap()));
        assert!(!units
            .iter()
            .any(|(date, _)| *date == "2026-10-10".parse::<NaiveDate>().unwrap()));
        assert_eq!(
            build_leave_date_units(&snapshot, employee, to, to, true, false, &HashSet::new())
                .unwrap()[0]
                .1,
            Decimal::new(5, 1)
        );
        assert_eq!(
            build_leave_date_units(&snapshot, employee, from, to, false, true, &HashSet::new())
                .unwrap()
                .len(),
            12
        );
        let persisted = serde_json::to_value(&units).unwrap();
        let mut changed = snapshot.clone();
        changed.policies.get_mut(&None).unwrap()[0]
            .rule
            .fixed_weekdays = vec![1, 2, 3, 4, 5];
        assert_ne!(
            build_leave_date_units(&changed, employee, from, to, false, false, &HashSet::new())
                .unwrap(),
            units
        );
        assert_eq!(
            serde_json::from_value::<Vec<(NaiveDate, Decimal)>>(persisted).unwrap(),
            units
        );
    }
}
