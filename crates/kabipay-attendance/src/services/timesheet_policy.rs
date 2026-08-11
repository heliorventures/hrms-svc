//! Editing rules for `timesheet_entry` rows from tenant-configurable lock policy.

use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0010_time_shift_roster::{
    timesheet_entry, timesheet_week_batch,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::services::{hrms_master_service, timesheet_dates};

pub const MAX_TIMESHEET_WEEK_HOURS: i64 = 40;

const BATCH_PENDING: &str = "PENDING";
const BATCH_APPROVED: &str = "APPROVED";

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

    let total = rows
        .into_iter()
        .fold(Decimal::ZERO, |sum, row| sum + row.hours_worked);
    if total > Decimal::from(MAX_TIMESHEET_WEEK_HOURS) {
        return Err(KabiPayError::Validation(format!(
            "weekly timesheet hours cannot exceed {} hours",
            MAX_TIMESHEET_WEEK_HOURS
        )));
    }

    Ok(())
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
    if policy.lock_approved_entries && (st == "APPROVED" || st == "SUBMITTED") {
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
