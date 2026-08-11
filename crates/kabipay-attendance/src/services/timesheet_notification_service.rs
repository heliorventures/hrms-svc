use chrono::{NaiveDate, Utc};
use kabipay_common::KabiPayResult;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

const TIMESHEET_REJECTED_NOTIFICATION_TYPE: &str = "TIMESHEET_REJECTED";
const TIMESHEET_REJECTED_TITLE: &str = "Timesheet rejected";
const TIMESHEET_ACTION_URL: &str = "/timesheet";

pub async fn notify_employee_timesheet_rejected(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    employee_id: Uuid,
    week_start: NaiveDate,
    rejection_reason: Option<&str>,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let Some(emp) = employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .one(txn)
        .await?
    else {
        return Ok(());
    };
    let Some(user_id) = emp.user_id else {
        return Ok(());
    };

    let reason = rejection_reason
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let message = match reason {
        Some(value) => format!(
            "Your timesheet for week starting {week_start} was rejected: {value}"
        ),
        None => format!(
            "Your timesheet for week starting {week_start} was rejected. Please review and resubmit."
        ),
    };

    notification::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        r#type: Set(Some(TIMESHEET_REJECTED_NOTIFICATION_TYPE.into())),
        title: Set(Some(TIMESHEET_REJECTED_TITLE.into())),
        message: Set(Some(message)),
        action_url: Set(Some(TIMESHEET_ACTION_URL.into())),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(txn)
    .await?;

    Ok(())
}
