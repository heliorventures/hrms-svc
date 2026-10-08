//! Version-one configuration has explicit safety semantics, not executable client rules.
use anyhow::{bail, Result};
use serde_json::Value;
fn exact_keys(value: &Value, keys: &[&str]) -> Result<()> {
    let Some(object) = value.as_object() else {
        bail!("PACKAGE_CONFIGURATION_INVALID");
    };
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        bail!("PACKAGE_CONFIGURATION_INVALID");
    }
    Ok(())
}
pub fn validate(value: &Value) -> Result<()> {
    exact_keys(value, &["leave", "payroll"])?;
    let leave = &value["leave"];
    let payroll = &value["payroll"];
    exact_keys(
        leave,
        &[
            "paid_type_code",
            "unpaid_type_code",
            "grant_mode",
            "unpaid_quota",
            "unpaid_max_consecutive_days",
            "approval",
        ],
    )?;
    exact_keys(
        payroll,
        &[
            "lwp_basis",
            "lwp_divisor",
            "rounding",
            "unresolved_future_rules",
            "auto_generate_payslips",
        ],
    )?;
    let codes = [
        leave["paid_type_code"].as_str(),
        leave["unpaid_type_code"].as_str(),
    ];
    if codes.iter().any(|code| {
        code.is_none_or(|code| {
            code.is_empty()
                || code.len() > 32
                || !code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
    }) || codes[0] == codes[1]
        || leave["grant_mode"] != "SNAPSHOT_ONLY"
        || leave["approval"] != "EXISTING_WORKFLOW"
        || !leave["unpaid_quota"].is_null()
        || !leave["unpaid_max_consecutive_days"].is_null()
        || payroll["lwp_basis"] != "GROSS"
        || payroll["rounding"] != "HALF_UP_2DP"
        || payroll["unresolved_future_rules"] != "REQUIRE_HR_CONFIGURATION"
        || payroll["auto_generate_payslips"] != false
    {
        bail!("PACKAGE_CONFIGURATION_INVALID");
    }
    if kabipay_payroll::services::payroll_rules::amount(
        payroll["lwp_divisor"].as_str(),
        "LWP divisor",
    )?
    .is_zero()
    {
        bail!("PACKAGE_CONFIGURATION_INVALID");
    }
    Ok(())
}
