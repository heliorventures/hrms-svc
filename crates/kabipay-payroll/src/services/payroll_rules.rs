//! Source-backed month calculations. Advances settle net earnings; they never change salary.
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, str::FromStr};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdditionalDeduction {
    pub code: String,
    pub amount: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeriodInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic: Option<super::automatic_payroll::AutomaticSettings>,
    pub year: i32,
    pub month: i32,
    pub gross_rule: String,
    pub fixed_gross: Option<String>,
    pub earned_gross_override: Option<String>,
    pub month_days: Option<String>,
    pub paid_days: Option<String>,
    pub present_days: Option<String>,
    pub lwp_days: Option<String>,
    pub source_lwp_days: Option<String>,
    pub lwp_divisor: Option<String>,
    pub lwp_amount_override: Option<String>,
    pub lwp_basis: Option<String>,
    pub lwp_handling: String,
    pub variable_allowance_ot: Option<String>,
    pub incentive: Option<String>,
    pub advance_already_paid: Option<String>,
    pub additional_deductions: Vec<AdditionalDeduction>,
    pub statutory_overrides: BTreeMap<String, Option<String>>,
    pub expected_earned_components: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub expected_wages: BTreeMap<String, Option<String>>,
    pub expected_employer_contributions: BTreeMap<String, Option<String>>,
    pub expected_statement: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub contribution_rules: serde_json::Value,
    pub ready: bool,
    #[serde(default)]
    pub status: Option<String>,
    pub historical_lwp_included: bool,
    #[serde(default)]
    pub approved_lwp_review_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalculatedPeriod {
    #[serde(with = "rust_decimal::serde::str")]
    pub gross: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub incentive: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_deductions: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_earned: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub advance_already_paid: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub remaining_payable: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub lwp_amount: Decimal,
    pub lwp_days: String,
    pub lwp_divisor: String,
    pub lwp_basis_amount: String,
    pub gross_rule: String,
    pub components: BTreeMap<String, String>,
    pub statutory: BTreeMap<String, String>,
    pub employer: BTreeMap<String, String>,
    pub additional_deductions: Vec<AdditionalDeduction>,
}

pub fn amount(raw: Option<&str>, field: &str) -> KabiPayResult<Decimal> {
    let value = raw.ok_or_else(|| KabiPayError::Validation(format!("{field} is unresolved")))?;
    let value = Decimal::from_str(value.trim())
        .map_err(|_| KabiPayError::Validation(format!("{field} must be a decimal")))?;
    if value.is_sign_negative() || value.scale() > 8 || value > Decimal::from(1_000_000_000_000_i64)
    {
        return Err(KabiPayError::Validation(format!(
            "{field} is outside the supported range"
        )));
    }
    Ok(value)
}

pub fn money(value: Decimal) -> Decimal {
    let mut rounded = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    rounded.rescale(2);
    rounded
}

fn supplied(map: &BTreeMap<String, Option<String>>, key: &str) -> KabiPayResult<Decimal> {
    amount(map.get(key).and_then(|v| v.as_deref()), key).map(money)
}

pub fn calculate_period(input: &PeriodInput) -> KabiPayResult<CalculatedPeriod> {
    if !(1900..=2200).contains(&input.year)
        || !(1..=12).contains(&input.month)
        || input.historical_lwp_included
        || input.lwp_handling != "SOURCE_GROSS_INCLUDES_REDUCTION"
    {
        return Err(KabiPayError::Validation(
            "invalid period or duplicate unpaid-leave attribution".into(),
        ));
    }
    let fixed = amount(input.fixed_gross.as_deref(), "fixed gross")?;
    let days = amount(input.lwp_days.as_deref(), "LWP days")?;
    let divisor = amount(input.lwp_divisor.as_deref(), "LWP divisor")?;
    if divisor.is_zero()
        || divisor > Decimal::from(366)
        || days > Decimal::from(31)
        || input.lwp_basis.as_deref() != Some("GROSS")
    {
        return Err(KabiPayError::Validation(
            "LWP requires gross basis, at most 31 days and a divisor between zero and 366".into(),
        ));
    }
    let lwp = match input.lwp_amount_override.as_deref() {
        Some(value) => money(amount(Some(value), "LWP override")?),
        None => money(fixed / divisor * days),
    };
    let gross = money(match input.gross_rule.as_str() {
        "FIXED_MINUS_LWP" => fixed - lwp,
        "PAID_DAYS_PLUS_OT" => {
            let month_days = amount(input.month_days.as_deref(), "month days")?;
            let paid_days = amount(input.paid_days.as_deref(), "paid days")?;
            if month_days.is_zero() || paid_days > month_days {
                return Err(KabiPayError::Validation(
                    "invalid paid-day proration".into(),
                ));
            }
            fixed / month_days * paid_days + amount(input.variable_allowance_ot.as_deref(), "OT")?
        }
        "SOURCE_OVERRIDE" => amount(
            input.earned_gross_override.as_deref(),
            "earned gross override",
        )?,
        _ => {
            return Err(KabiPayError::Validation(
                "unsupported period gross rule".into(),
            ))
        }
    });
    let incentive = money(amount(input.incentive.as_deref(), "incentive")?);
    let advance = money(amount(
        input.advance_already_paid.as_deref(),
        "advance already paid",
    )?);
    let mut additional = Decimal::ZERO;
    let mut codes = std::collections::HashSet::new();
    for item in &input.additional_deductions {
        if !valid_additional_code(&item.code) || !codes.insert(&item.code) {
            return Err(KabiPayError::Validation(
                "additional deduction code is invalid, duplicated or reserved".into(),
            ));
        }
        let value = amount(item.amount.as_deref(), "additional deduction")?;
        if value > Decimal::ZERO
            && item
                .reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(KabiPayError::Validation(
                "additional deduction reason is required".into(),
            ));
        }
        additional += money(value);
    }
    let statutory = ["PF", "ESI", "PT", "TDS"]
        .into_iter()
        .map(|code| {
            supplied(&input.statutory_overrides, code)
                .map(|value| (code.to_string(), value.to_string()))
        })
        .collect::<KabiPayResult<BTreeMap<_, _>>>()?;
    let statutory_total = ["PF", "ESI", "PT", "TDS"]
        .into_iter()
        .map(|key| supplied(&input.statutory_overrides, key))
        .collect::<KabiPayResult<Vec<_>>>()?
        .into_iter()
        .sum::<Decimal>();
    let total = statutory_total + additional;
    let net = gross + incentive - total;
    let remaining = net - advance;
    if gross.is_sign_negative() || net.is_sign_negative() || remaining.is_sign_negative() {
        return Err(KabiPayError::Validation(
            "salary or advance credit requires reviewed settlement".into(),
        ));
    }
    let components = input
        .expected_earned_components
        .iter()
        .map(|(key, value)| {
            amount(value.as_deref(), "earned component")
                .map(|v| (key.clone(), money(v).to_string()))
        })
        .collect::<KabiPayResult<BTreeMap<_, _>>>()?;
    if !components.is_empty() {
        let sum = input
            .expected_earned_components
            .keys()
            .map(|key| supplied(&input.expected_earned_components, key))
            .collect::<KabiPayResult<Vec<_>>>()?
            .into_iter()
            .sum::<Decimal>();
        if (sum - gross).abs() > Decimal::new(2, 2) {
            return Err(KabiPayError::Validation(
                "earned components do not reconcile to gross".into(),
            ));
        }
    }
    for (key, actual) in [
        ("earned_gross", gross),
        ("statutory_total", statutory_total),
        ("additional_total", additional),
        ("net_earned", net),
        ("remaining_payable", remaining),
    ] {
        if let Some(Some(expected)) = input.expected_statement.get(key) {
            if (amount(Some(expected), key)? - actual).abs() > Decimal::new(2, 2) {
                return Err(KabiPayError::Validation(format!(
                    "source reconciliation failed for {key}"
                )));
            }
        }
    }
    let employer = ["pf", "esi"]
        .into_iter()
        .filter(|key| {
            input
                .expected_employer_contributions
                .get(*key)
                .is_some_and(Option::is_some)
        })
        .map(|key| {
            supplied(&input.expected_employer_contributions, key)
                .map(|value| (key.to_string(), value.to_string()))
        })
        .collect::<KabiPayResult<BTreeMap<_, _>>>()?;
    Ok(CalculatedPeriod {
        gross,
        incentive,
        total_deductions: total,
        net_earned: net,
        advance_already_paid: advance,
        remaining_payable: remaining,
        lwp_amount: lwp,
        lwp_days: days.to_string(),
        lwp_divisor: divisor.to_string(),
        lwp_basis_amount: fixed.to_string(),
        gross_rule: input.gross_rule.clone(),
        components,
        statutory,
        employer,
        additional_deductions: input.additional_deductions.clone(),
    })
}

pub fn valid_additional_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.as_bytes()[0].is_ascii_uppercase()
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !matches!(
            code,
            "ADVANCE" | "LWP" | "UNPAID_LEAVE" | "PF" | "ESI" | "PT" | "TDS"
        )
}
