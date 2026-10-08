//! Reconcile approved dated LWP with source aggregate totals without a second charge.
use super::payroll_rules::{amount, PeriodInput};
use chrono::{NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0077_unpaid_leave_payroll::payroll_unpaid_leave_policy;
use rust_decimal::Decimal;
use sea_orm::ConnectionTrait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub async fn review<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
    month: i32,
    freeze: bool,
) -> KabiPayResult<serde_json::Value> {
    let date = NaiveDate::from_ymd_opt(year, month as u32, 1)
        .filter(|_| (1900..=2200).contains(&year))
        .ok_or_else(|| KabiPayError::Validation("invalid payroll period".into()))?;
    let now = Utc::now();
    let policy = payroll_unpaid_leave_policy::Model {
        tenant_id: tenant,
        enabled: true,
        basic_component_code: Some("GROSS".into()),
        day_divisor: Some(Decimal::from(31)),
        treatment: Some(super::unpaid_leave_policy::BEFORE.into()),
        updated_by: Uuid::nil(),
        created_at: now,
        updated_at: now,
    };
    let allocation = super::unpaid_leave_policy::calculate_with_mode(
        db,
        tenant,
        employee,
        date,
        &policy,
        Decimal::from(31),
        freeze,
    )
    .await?;
    let bytes = serde_json::to_vec(&allocation.source_days)
        .map_err(|_| KabiPayError::Internal("LWP review serialization failed".into()))?;
    let hash = if allocation.days.is_zero() {
        None
    } else {
        Some(hex::encode(Sha256::digest(bytes)))
    };
    Ok(
        serde_json::json!({"hash":hash,"days":allocation.days.to_string(),"source_days":allocation.source_days}),
    )
}

pub async fn validate<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    input: &PeriodInput,
    freeze: bool,
) -> KabiPayResult<serde_json::Value> {
    let current = review(db, tenant, employee, input.year, input.month, freeze).await?;
    if current["hash"].as_str() != input.approved_lwp_review_hash.as_deref() {
        return Err(KabiPayError::Validation(
            "approved dated LWP changed or has not been reviewed for this month's aggregate total"
                .into(),
        ));
    }
    let approved = amount(current["days"].as_str(), "approved dated LWP")?;
    if approved > amount(input.lwp_days.as_deref(), "period LWP")? {
        return Err(KabiPayError::Validation(
            "monthly LWP total is smaller than approved dated LWP".into(),
        ));
    }
    Ok(current)
}
