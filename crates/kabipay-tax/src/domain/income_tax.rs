use super::{tax_rules_india, validate_amount, TaxRegime};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomeTaxInput {
    pub fiscal_year: i32,
    pub regime: TaxRegime,
    pub gross: Decimal,
    pub approved_deductions: Decimal,
    pub resident: Option<bool>,
    pub age_at_year_end: Option<u32>,
    pub special_rate_income: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlabAmount {
    pub from: Decimal,
    pub to: Decimal,
    pub rate: Decimal,
    pub tax: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomeTaxResult {
    pub rule_version: String,
    pub source: String,
    pub gross: Decimal,
    pub standard_deduction: Decimal,
    pub permitted_deductions: Decimal,
    pub taxable_income: Decimal,
    pub statutory_taxable_income: Decimal,
    pub slabs: Vec<SlabAmount>,
    pub slab_tax: Decimal,
    pub rebate: Decimal,
    pub surcharge: Decimal,
    pub marginal_relief: Decimal,
    pub cess: Decimal,
    pub display_tax: Decimal,
    pub statutory_tax: Decimal,
}
pub fn statutory_round(value: Decimal) -> Decimal {
    (value.trunc() / Decimal::TEN).round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        * Decimal::TEN
}
fn slabs(income: Decimal, bands: &[(i64, i64)]) -> Vec<SlabAmount> {
    let mut lower = Decimal::ZERO;
    bands
        .iter()
        .map(|(ceiling, rate)| {
            let upper = Decimal::from(*ceiling);
            let fraction = Decimal::new(*rate, 2);
            let amount = SlabAmount {
                from: lower,
                to: upper,
                rate: fraction,
                tax: (income.min(upper) - lower).max(Decimal::ZERO) * fraction,
            };
            lower = upper;
            amount
        })
        .collect()
}
fn surcharge_rate(income: Decimal, regime: TaxRegime) -> (Decimal, Decimal, Decimal) {
    let bands = if regime == TaxRegime::New {
        vec![(5000000, 10, 0), (10000000, 15, 10), (20000000, 25, 15)]
    } else {
        vec![
            (5000000, 10, 0),
            (10000000, 15, 10),
            (20000000, 25, 15),
            (50000000, 37, 25),
        ]
    };
    bands
        .into_iter()
        .rev()
        .find(|(threshold, _, _)| income > Decimal::from(*threshold))
        .map(|(t, r, p)| (Decimal::from(t), Decimal::new(r, 2), Decimal::new(p, 2)))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO))
}
fn tax_parts(
    income: Decimal,
    bands: &[(i64, i64)],
    regime: TaxRegime,
    resident: bool,
) -> (Vec<SlabAmount>, Decimal, Decimal, Decimal, Decimal) {
    let rows = slabs(income, bands);
    let gross: Decimal = rows.iter().map(|r| r.tax).sum();
    let rebate = if resident && regime == TaxRegime::New && income <= Decimal::from(1200000) {
        gross.min(Decimal::from(60000))
    } else if resident && regime == TaxRegime::Old && income <= Decimal::from(500000) {
        gross.min(Decimal::from(12500))
    } else {
        Decimal::ZERO
    };
    let rebate_relief = if resident && regime == TaxRegime::New && income > Decimal::from(1200000) {
        (gross - (income - Decimal::from(1200000))).max(Decimal::ZERO)
    } else {
        Decimal::ZERO
    };
    let (threshold, rate, previous) = surcharge_rate(income, regime);
    let surcharge = (gross - rebate - rebate_relief) * rate;
    let surcharge_relief = if rate > Decimal::ZERO {
        let threshold_tax: Decimal = slabs(threshold, bands).iter().map(|r| r.tax).sum();
        (gross + surcharge - (threshold_tax * (Decimal::ONE + previous) + income - threshold))
            .max(Decimal::ZERO)
    } else {
        Decimal::ZERO
    };
    (
        rows,
        rebate,
        surcharge,
        rebate_relief + surcharge_relief,
        gross,
    )
}
pub fn calculate_income_tax(input: &IncomeTaxInput) -> KabiPayResult<IncomeTaxResult> {
    validate_amount(input.gross)?;
    validate_amount(input.approved_deductions)?;
    let resident = input.resident.ok_or_else(|| {
        KabiPayError::Validation("tax residency not provided; HR review required".into())
    })?;
    if input.fiscal_year != 2026 || input.special_rate_income {
        return Err(KabiPayError::Validation(
            "tax rules do not support this year or special-rate income; HR review required".into(),
        ));
    }
    let exemption = if input.regime == TaxRegime::Old && resident {
        match input.age_at_year_end {
            Some(age) if age >= 80 => 500000,
            Some(age) if age >= 60 => 300000,
            Some(_) => 250000,
            None => {
                return Err(KabiPayError::Validation(
                    "age is required for old-regime thresholds".into(),
                ))
            }
        }
    } else {
        250000
    };
    let old = [
        (exemption, 0),
        (500000, 5),
        (1000000, 20),
        (1_000_000_000_000, 30),
    ];
    let bands = if input.regime == TaxRegime::New {
        tax_rules_india::NEW_BANDS
    } else {
        &old
    };
    let standard = input
        .gross
        .min(Decimal::from(if input.regime == TaxRegime::New {
            75000
        } else {
            50000
        }));
    let permitted = if input.regime == TaxRegime::Old {
        input.approved_deductions.min(input.gross - standard)
    } else {
        Decimal::ZERO
    };
    let income = (input.gross - standard - permitted).max(Decimal::ZERO);
    let (rows, rebate, surcharge, relief, gross) = tax_parts(income, bands, input.regime, resident);
    let cess = (gross - rebate + surcharge - relief) * Decimal::new(4, 2);
    let rounded_income = statutory_round(income);
    let (_, r, s, m, g) = tax_parts(rounded_income, bands, input.regime, resident);
    Ok(IncomeTaxResult {
        rule_version: tax_rules_india::RULE_VERSION.into(),
        source: tax_rules_india::SOURCE.into(),
        gross: input.gross,
        standard_deduction: standard,
        permitted_deductions: permitted,
        taxable_income: income,
        statutory_taxable_income: rounded_income,
        slabs: rows,
        slab_tax: gross,
        rebate,
        surcharge,
        marginal_relief: relief,
        cess,
        display_tax: (gross - rebate + surcharge - relief + cess)
            .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero),
        statutory_tax: statutory_round((g - r + s - m) * Decimal::new(104, 2)),
    })
}
