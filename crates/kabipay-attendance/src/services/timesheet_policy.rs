//! Editing rules for `timesheet_entry` rows from tenant-configurable lock policy.

use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0010_time_shift_roster::{
    timesheet_entry, timesheet_week_batch,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::collections::HashMap;
use uuid::Uuid;

use crate::services::{hrms_master_service, timesheet_dates};

pub const MAX_TIMESHEET_WEEK_HOURS: i64 = 40;
pub const MAX_TIMESHEET_DAY_HOURS: i64 = 24;

const BATCH_PENDING: &str = "PENDING";
const BATCH_APPROVED: &str = "APPROVED";
const TASK_PREFIX: &str = "[task:";

pub fn description_has_task_type(description: Option<&str>) -> bool {
    let Some(description) = description else {
        return false;
    };
    let trimmed = description.trim();
    if !trimmed.starts_with(TASK_PREFIX) {
        return false;
    }
    let Some(end) = trimmed[TASK_PREFIX.len()..].find(']') else {
        return false;
    };
    !trimmed[TASK_PREFIX.len()..TASK_PREFIX.len() + end]
        .trim()
        .is_empty()
}

pub fn assert_required_project_and_task(
    project_code: Option<&str>,
    description: Option<&str>,
) -> KabiPayResult<()> {
    if project_code.map(str::trim).unwrap_or_default().is_empty() {
        return Err(KabiPayError::Validation("project is required for timesheet entries".into()));
    }
    if !description_has_task_type(description) {
        return Err(KabiPayError::Validation(
            "task type is required for timesheet entries".into(),
        ));
    }
    Ok(())
}

/// Earliest Monday start date employees may still **create/edit drafts** for, inclusive.
pub fn earliest_editable_week_start(policy: &hrms_master_service::TimesheetLockPolicy) -> NaiveDate {
    timesheet_dates::earliest_editable_week_start(policy)
}

pub async fn assert_work_date_allowed_for_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    work_date: NaiveDate,
) -> KabiPayResult<()> {
    let policy = hrms_master_service::load_timesheet_lock_policy(db, tenant_id).await?;
    let min_week_mon = earliest_editable_week_start(&policy);
    let (week_mon, _) = timesheet_dates::week_monday_sunday(work_date);
    if week_mon < min_week_mon {
        return Err(KabiPayError::Validation(format!(
            "timesheet entries cannot be edited for weeks before {} — adjust HR lock policy if needed",
            min_week_mon
        )));
    }
    Ok(())
}

pub async fn assert_week_has_no_active_submission(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
) -> KabiPayResult<()> {
    let (week_mon, _) = timesheet_dates::week_monday_sunday(work_date);
    let active = timesheet_week_batch::Entity::find()
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .filter(timesheet_week_batch::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_week_batch::Column::WeekStartDate.eq(week_mon))
        .filter(timesheet_week_batch::Column::Status.is_in(vec![
            BATCH_PENDING.to_string(),
            BATCH_APPROVED.to_string(),
        ]))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    if active.is_some() {
        return Err(KabiPayError::Validation(
            "this week already has a pending or approved timesheet submission".into(),
        ));
    }

    Ok(())
}

pub async fn assert_week_hours_with_entry_change(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
    entry_id_to_replace: Option<Uuid>,
    next_hours: Decimal,
) -> KabiPayResult<()> {
    let (week_mon, week_sun) = timesheet_dates::week_monday_sunday(work_date);
    let rows = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .filter(timesheet_entry::Column::WorkDate.gte(week_mon))
        .filter(timesheet_entry::Column::WorkDate.lte(week_sun))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut total = next_hours;
    for row in rows {
        if Some(row.id) == entry_id_to_replace {
            continue;
        }
        total += row.hours_worked;
    }

    if total > Decimal::from(MAX_TIMESHEET_WEEK_HOURS) {
        return Err(KabiPayError::Validation(format!(
            "weekly timesheet hours cannot exceed {} hours",
            MAX_TIMESHEET_WEEK_HOURS
        )));
    }

    Ok(())
}

pub async fn assert_day_hours_with_entry_change(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
    entry_id_to_replace: Option<Uuid>,
    next_hours: Decimal,
) -> KabiPayResult<()> {
    let rows = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .filter(timesheet_entry::Column::WorkDate.eq(work_date))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut total = next_hours;
    for row in rows {
        if Some(row.id) == entry_id_to_replace {
            continue;
        }
        total += row.hours_worked;
    }

    if total > Decimal::from(MAX_TIMESHEET_DAY_HOURS) {
        return Err(KabiPayError::Validation(format!(
            "daily timesheet hours cannot exceed {} hours",
            MAX_TIMESHEET_DAY_HOURS
        )));
    }

    Ok(())
}

pub async fn assert_week_hours_for_submission(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    week_start: NaiveDate,
) -> KabiPayResult<()> {
    let (week_mon, week_sun) = timesheet_dates::week_monday_sunday(week_start);
    let rows = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .filter(timesheet_entry::Column::WorkDate.gte(week_mon))
        .filter(timesheet_entry::Column::WorkDate.lte(week_sun))
        .filter(timesheet_entry::Column::Status.eq("DRAFT"))
        .filter(timesheet_entry::Column::BatchId.is_null())
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut daily_totals: HashMap<NaiveDate, Decimal> = HashMap::new();
    let total = rows.into_iter().fold(Decimal::ZERO, |sum, row| {
        *daily_totals.entry(row.work_date).or_insert(Decimal::ZERO) += row.hours_worked;
        sum + row.hours_worked
    });
    if daily_totals
        .values()
        .any(|day_total| *day_total > Decimal::from(MAX_TIMESHEET_DAY_HOURS))
    {
        return Err(KabiPayError::Validation(format!(
            "daily timesheet hours cannot exceed {} hours",
            MAX_TIMESHEET_DAY_HOURS
        )));
    }
    if total > Decimal::from(MAX_TIMESHEET_WEEK_HOURS) {
        return Err(KabiPayError::Validation(format!(
            "weekly timesheet hours cannot exceed {} hours",
            MAX_TIMESHEET_WEEK_HOURS
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_has_task_type_requires_encoded_task_marker() {
        assert!(description_has_task_type(Some("[task:DEV] implementation")));
        assert!(!description_has_task_type(Some("implementation only")));
        assert!(!description_has_task_type(Some("[task:] implementation")));
        assert!(!description_has_task_type(None));
    }

    #[test]
    fn project_and_task_are_required_for_timesheet_entries() {
        assert!(assert_required_project_and_task(Some("CLIENT"), Some("[task:DEV]")).is_ok());
        assert!(assert_required_project_and_task(None, Some("[task:DEV]")).is_err());
        assert!(assert_required_project_and_task(Some("CLIENT"), Some("notes only")).is_err());
    }
}

pub async fn assert_entry_mut_allowed(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    row: &timesheet_entry::Model,
) -> KabiPayResult<()> {
    if row.is_deleted {
        return Err(KabiPayError::Validation("timesheet entry was deleted".into()));
    }
    let st = row.status.trim().to_uppercase();
    let policy = hrms_master_service::load_timesheet_lock_policy(db, tenant_id).await?;
    if st == "SUBMITTED" {
        return Err(KabiPayError::Validation(
            "submitted timesheet rows cannot be edited - reject the week submission first".into(),
        ));
    }
    if policy.lock_approved_entries && st == "APPROVED" {
        return Err(KabiPayError::Validation(
            "approved or submitted timesheet rows cannot be edited — reject the week submission first"
                .into(),
        ));
    }
    if st == "DRAFT" && row.batch_id.is_some() {
        return Err(KabiPayError::Validation(
            "draft row is linked to a batch — unexpected state".into(),
        ));
    }
    assert_work_date_allowed_for_entry(db, tenant_id, row.work_date).await?;
    Ok(())
}
