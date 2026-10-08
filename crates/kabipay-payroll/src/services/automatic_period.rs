//! Routine periods derive from effective configuration; only explicit exceptions are stored.
use super::payroll_rules::{amount, PeriodInput};
use chrono::Datelike;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LwpOverride {
    pub days: String,
    pub reason: String,
}

pub fn new_input(year: i32, month: i32) -> KabiPayResult<PeriodInput> {
    let (_, end) = kabipay_tax::domain::tax_year::month_bounds(year, month as u32)?;
    serde_json::from_value(serde_json::json!({
        "year":year,"month":month,"gross_rule":"FIXED_MINUS_LWP",
        "month_days":end.day().to_string(),"lwp_days":"0","lwp_basis":"GROSS",
        "lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION",
        "variable_allowance_ot":"0","incentive":"0","advance_already_paid":"0",
        "additional_deductions":[],"statutory_overrides":{},"expected_earned_components":{},
        "expected_employer_contributions":{},"expected_statement":{},
        "historical_lwp_included":false,"ready":true,
        "automatic":{"use_employee_configuration":true,"eligibility":{
            "pf_applicable":null,"esi_applicable":null,"esi_continuation_until":null,
            "disability":null,"average_daily_wage":null
        },"withholding_override":null}
    }))
    .map_err(|_| KabiPayError::Internal("cannot prepare automatic month".into()))
}

pub fn refresh_lwp(input: &mut PeriodInput, review: &serde_json::Value) -> KabiPayResult<()> {
    let approved = amount(review["days"].as_str(), "approved unpaid leave")?;
    let override_value = input
        .automatic
        .as_ref()
        .and_then(|value| value.lwp_override.as_ref());
    let days = match override_value {
        Some(value) => {
            kabipay_tax::domain::validate_reason(Some(&value.reason))?;
            let days = amount(Some(&value.days), "unpaid leave override")?;
            if days < approved || days > rust_decimal::Decimal::from(31) {
                return Err(KabiPayError::Validation(
                    "unpaid leave override must cover approved days and cannot exceed 31".into(),
                ));
            }
            days
        }
        None => approved,
    };
    input.lwp_days = Some(days.to_string());
    input.approved_lwp_review_hash = review["hash"].as_str().map(str::to_owned);
    Ok(())
}

pub async fn resolve<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    input: &PeriodInput,
) -> KabiPayResult<PeriodInput> {
    let mut resolved = input.clone();
    if !input
        .automatic
        .as_ref()
        .is_some_and(|value| value.use_employee_configuration)
    {
        return Ok(resolved);
    }
    let (start, _) = kabipay_tax::domain::tax_year::month_bounds(input.year, input.month as u32)?;
    let configured = super::employee_eligibility::find(db, tenant, employee, start)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation(
                "confirm employee PF/ESI eligibility in Employee payroll settings".into(),
            )
        })?;
    if let Some(automatic) = &mut resolved.automatic {
        automatic.eligibility = configured.eligibility;
    }
    let review =
        super::imported_lwp::review(db, tenant, employee, input.year, input.month, false).await?;
    refresh_lwp(&mut resolved, &review)?;
    Ok(resolved)
}
