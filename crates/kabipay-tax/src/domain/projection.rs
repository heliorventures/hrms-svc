use super::income_tax::{calculate_income_tax, IncomeTaxInput, IncomeTaxResult};
use super::withholding::{allocate, WithholdingAllocation};
use super::{
    tax_year::{employment_months, month_bounds},
    validate_amount, EvidenceKind, TaxSettingsInput, WithholdingMethod,
};
use chrono::{Datelike, Duration, NaiveDate};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SalaryPeriod {
    pub from: NaiveDate,
    pub until: Option<NaiveDate>,
    pub components: BTreeMap<String, Decimal>,
    pub divisor: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActualMonth {
    pub year: i32,
    pub month: u32,
    pub components: BTreeMap<String, Decimal>,
    pub tds: Option<Decimal>,
    pub evidence: EvidenceKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionMonth {
    pub year: i32,
    pub month: u32,
    pub components: BTreeMap<String, Decimal>,
    pub earnings: Decimal,
    pub tds: Option<Decimal>,
    pub evidence: EvidenceKind,
    pub aggregate_source: Option<String>,
    #[serde(default)]
    pub projected_withholding: Option<Decimal>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaxProjectionInput {
    pub fiscal_year: i32,
    pub joining: NaiveDate,
    pub exit: Option<NaiveDate>,
    pub as_of: NaiveDate,
    pub salaries: Vec<SalaryPeriod>,
    pub actuals: Vec<ActualMonth>,
    pub settings: TaxSettingsInput,
    pub approved_deductions: Decimal,
    pub resident: Option<bool>,
    pub age_at_year_end: Option<u32>,
    pub previous_employer_earnings: Decimal,
    pub previous_employer_tds: Option<Decimal>,
    #[serde(default)]
    pub previous_employer_history_complete: Option<bool>,
    pub taxable_component_codes: BTreeSet<String>,
    #[serde(default)]
    pub opening_history: Vec<super::TaxHistoryEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaxProjection {
    pub fiscal_year: i32,
    #[serde(default)]
    pub configuration: Option<ProjectionConfiguration>,
    pub months: Vec<ProjectionMonth>,
    pub annual_earnings: Decimal,
    pub tax: Option<IncomeTaxResult>,
    pub withholding: Option<WithholdingAllocation>,
    pub recorded_tds: Decimal,
    pub history_complete: bool,
    pub selected_monthly_tds: Option<Decimal>,
    pub limitations: Vec<String>,
    pub note: String,
    pub opening_history: Vec<super::TaxHistoryEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionConfiguration {
    pub regime: super::TaxRegime,
    pub method: WithholdingMethod,
    pub percentage: Option<Decimal>,
    pub basis_components: Vec<String>,
    pub effective_from: NaiveDate,
}
fn money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}
pub fn project_months(
    year: i32,
    joining: NaiveDate,
    exit: Option<NaiveDate>,
    as_of: NaiveDate,
    salaries: &[SalaryPeriod],
    actuals: &[ActualMonth],
) -> KabiPayResult<Vec<ProjectionMonth>> {
    project_months_with_history(year, joining, exit, as_of, salaries, actuals, &[])
}

/// Project only the requested payroll month, independent of earlier salary assignments.
pub fn project_payroll_month(
    year: i32,
    month: u32,
    joining: NaiveDate,
    exit: Option<NaiveDate>,
    salaries: &[SalaryPeriod],
) -> KabiPayResult<ProjectionMonth> {
    let (start, end) = month_bounds(year, month)?;
    if joining > end || exit.is_some_and(|date| date < start) {
        return Err(KabiPayError::Validation(
            "employee is outside employment for this month".into(),
        ));
    }
    project_months(
        year - i32::from(month < 4),
        joining.max(start),
        Some(exit.unwrap_or(end).min(end)),
        start,
        salaries,
        &[],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| KabiPayError::Validation("employee is outside employment for this month".into()))
}

fn project_months_with_history(
    year: i32,
    joining: NaiveDate,
    exit: Option<NaiveDate>,
    as_of: NaiveDate,
    salaries: &[SalaryPeriod],
    actuals: &[ActualMonth],
    history: &[super::TaxHistoryEntry],
) -> KabiPayResult<Vec<ProjectionMonth>> {
    let mut starts = BTreeSet::new();
    for salary in salaries {
        if !starts.insert(salary.from)
            || salary.until.is_some_and(|v| v < salary.from)
            || salary.divisor.is_some_and(|v| !(1..=31).contains(&v))
        {
            return Err(KabiPayError::Validation(
                "invalid or duplicate dated salary assignment".into(),
            ));
        }
        for amount in salary.components.values() {
            validate_amount(*amount)?;
        }
    }
    let mut actual_keys = BTreeSet::new();
    for actual in actuals {
        month_bounds(actual.year, actual.month)?;
        let current_preview = actual.evidence == EvidenceKind::FutureProjection
            && actual.year == as_of.year()
            && actual.month == as_of.month()
            && actual.tds.is_none();
        if !actual_keys.insert((actual.year, actual.month))
            || (!matches!(
                actual.evidence,
                EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
            ) && !current_preview)
        {
            return Err(KabiPayError::Validation(
                "duplicate month or estimated data supplied as actual".into(),
            ));
        }
        for amount in actual.components.values() {
            validate_amount(*amount)?;
        }
        if let Some(tds) = actual.tds {
            validate_amount(tds)?;
        }
    }
    let mut rows = Vec::new();
    for period in employment_months(year, joining, exit)? {
        let key = (period.start.year(), period.start.month());
        if let Some(actual) = actuals.iter().find(|v| (v.year, v.month) == key) {
            rows.push(ProjectionMonth {
                year: key.0,
                month: key.1,
                components: actual.components.clone(),
                earnings: actual.components.values().copied().sum(),
                tds: actual.tds,
                evidence: actual.evidence,
                aggregate_source: None,
                projected_withholding: None,
            });
            continue;
        }
        let (month_start, month_end) = month_bounds(key.0, key.1)?;
        if history
            .iter()
            .any(|entry| entry.period_start <= period.start && entry.period_end >= period.end)
        {
            // Coverage is validated below. Do not fabricate monthly amounts or require
            // historical salary assignments for earnings supplied as an aggregate.
            rows.push(ProjectionMonth {
                year: key.0,
                month: key.1,
                components: BTreeMap::new(),
                earnings: Decimal::ZERO,
                tds: None,
                evidence: EvidenceKind::HistoricalEstimate,
                aggregate_source: None,
                projected_withholding: None,
            });
            continue;
        }
        let mut components = BTreeMap::new();
        let mut day = period.start;
        while day <= period.end {
            let salary = salaries
                .iter()
                .filter(|v| v.from <= day && v.until.is_none_or(|end| end >= day))
                .max_by_key(|v| v.from)
                .ok_or_else(|| {
                    KabiPayError::Validation(
                        "salary assignment is missing for an employment period".into(),
                    )
                })?;
            let full_month = period.start == month_start
                && period.end == month_end
                && salary.from <= month_start
                && salary.until.is_none_or(|end| end >= month_end)
                && !salaries
                    .iter()
                    .any(|v| v.from > month_start && v.from <= month_end);
            let denominator = Decimal::from(if full_month {
                period.calendar_days
            } else {
                salary.divisor.unwrap_or(period.calendar_days)
            });
            for (code, amount) in &salary.components {
                *components.entry(code.clone()).or_insert(Decimal::ZERO) += *amount / denominator;
            }
            day += Duration::days(1);
        }
        for amount in components.values_mut() {
            *amount = money(*amount);
        }
        rows.push(ProjectionMonth {
            year: key.0,
            month: key.1,
            earnings: components.values().copied().sum(),
            components,
            tds: None,
            aggregate_source: None,
            projected_withholding: None,
            evidence: if month_end < as_of {
                EvidenceKind::HistoricalEstimate
            } else {
                EvidenceKind::FutureProjection
            },
        });
    }
    Ok(rows)
}
pub fn calculate_projection(input: &TaxProjectionInput) -> KabiPayResult<TaxProjection> {
    input.settings.validate()?;
    validate_amount(input.previous_employer_earnings)?;
    let mut months = project_months_with_history(
        input.fiscal_year,
        input.joining,
        input.exit,
        input.as_of,
        &input.salaries,
        &input.actuals,
        &input.opening_history,
    )?;
    let mut history_earnings = Decimal::ZERO;
    let mut history_taxable = Decimal::ZERO;
    let mut history_tds = Decimal::ZERO;
    for entry in &input.opening_history {
        entry.validate()?;
        if entry.fiscal_year != input.fiscal_year
            || entry.period_end >= input.as_of
            || entry.employer != "CURRENT"
        {
            return Err(KabiPayError::Validation(
                "opening coverage must be prior current-employer history in this tax year".into(),
            ));
        }
        let (_, last) = month_bounds(entry.period_end.year(), entry.period_end.month())?;
        if (entry.period_start.day() != 1 && entry.period_start != input.joining)
            || (entry.period_end != last && Some(entry.period_end) != input.exit)
        {
            return Err(KabiPayError::Validation(
                "partial-month opening history requires a monthly earnings breakdown".into(),
            ));
        }
        let mut covered = 0;
        for month in &mut months {
            let (start, end) = month_bounds(month.year, month.month)?;
            if start <= entry.period_end && end >= entry.period_start {
                if month.aggregate_source.is_some()
                    || matches!(
                        month.evidence,
                        EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
                    )
                {
                    return Err(KabiPayError::Validation(
                        "opening history overlaps another actual source".into(),
                    ));
                }
                // No monthly distribution is invented from an aggregate source.
                // UI shows the covered range total separately and hides these estimates.
                month.aggregate_source = Some(entry.source_key.clone());
                covered += 1;
            }
        }
        if covered == 0 {
            return Err(KabiPayError::Validation(
                "opening history does not cover employment".into(),
            ));
        }
        history_earnings += entry.earnings;
        history_taxable += entry
            .components
            .iter()
            .filter(|(code, _)| input.taxable_component_codes.contains(*code))
            .map(|(_, v)| *v)
            .sum::<Decimal>();
        history_tds += entry.tds.unwrap_or(Decimal::ZERO);
    }
    let prior: Vec<_> = months
        .iter()
        .filter(|m| month_bounds(m.year, m.month).is_ok_and(|(_, end)| end < input.as_of))
        .collect();
    let history_complete = prior.iter().all(|m| {
        m.aggregate_source.as_ref().is_some_and(|key| {
            input.opening_history.iter().any(|h| {
                &h.source_key == key
                    && h.coverage == super::CoverageStatus::Complete
                    && h.tds.is_some()
            })
        }) || (m.tds.is_some()
            && matches!(
                m.evidence,
                EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
            ))
    }) && input.previous_employer_history_complete.unwrap_or(
        input.previous_employer_earnings.is_zero() && input.previous_employer_tds.is_none(),
    ) && (input.previous_employer_earnings.is_zero()
        || input.previous_employer_tds.is_some())
        && months
            .iter()
            .filter(|m| {
                matches!(
                    m.evidence,
                    EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
                )
            })
            .all(|m| m.tds.is_some());
    let recorded_tds = months
        .iter()
        .filter(|m| {
            matches!(
                m.evidence,
                EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
            )
        })
        .filter_map(|m| m.tds)
        .sum::<Decimal>()
        + input.previous_employer_tds.unwrap_or(Decimal::ZERO)
        + history_tds;
    let annual_earnings = months
        .iter()
        .filter(|m| m.aggregate_source.is_none())
        .map(|m| m.earnings)
        .sum::<Decimal>()
        + input.previous_employer_earnings
        + history_earnings;
    let annual_taxable = months
        .iter()
        .filter(|m| m.aggregate_source.is_none())
        .flat_map(|m| m.components.iter())
        .filter(|(code, _)| input.taxable_component_codes.contains(*code))
        .map(|(_, v)| *v)
        .sum::<Decimal>()
        + input.previous_employer_earnings
        + history_taxable;
    let mut limitations =
        vec!["Future variable incentives and revisions are excluded unless supplied.".into()];
    if !history_complete {
        limitations.push(
            "Historical TDS not provided. Known payroll deductions are a partial total.".into(),
        );
    }
    let tax = match calculate_income_tax(&IncomeTaxInput {
        fiscal_year: input.fiscal_year,
        regime: input.settings.regime,
        gross: annual_taxable,
        approved_deductions: input.approved_deductions,
        resident: input.resident,
        age_at_year_end: input.age_at_year_end,
        special_rate_income: false,
    }) {
        Ok(tax) => Some(tax),
        Err(error) => {
            limitations.push(error.to_string());
            None
        }
    };
    let remaining = months
        .iter()
        .filter(|m| {
            month_bounds(m.year, m.month).is_ok_and(|(_, end)| end >= input.as_of)
                && !matches!(
                    m.evidence,
                    EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
                )
        })
        .count() as u32;
    let withholding = tax
        .as_ref()
        .filter(|_| remaining > 0)
        .map(|tax| {
            allocate(
                tax.statutory_tax,
                history_complete.then_some(recorded_tds),
                months.len() as u32,
                remaining,
            )
        })
        .transpose()?;
    if withholding
        .as_ref()
        .is_some_and(|value| value.excess_withholding)
    {
        limitations.push("Recorded TDS exceeds projected annual tax. HR must review future withholding; this projection does not create a refund.".into());
    }
    let recorded_current = months.iter().find(|m| {
        m.year == input.as_of.year()
            && m.month == input.as_of.month()
            && matches!(
                m.evidence,
                EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
            )
    });
    let selected_monthly_tds = if let Some(month) = recorded_current {
        month.tds
    } else if input.settings.method == WithholdingMethod::PercentageOverride {
        let current = months
            .iter()
            .find(|m| m.year == input.as_of.year() && m.month == input.as_of.month());
        current
            .map(|month| {
                let mut basis = Decimal::ZERO;
                for code in &input.settings.basis_components {
                    basis += month.components.get(code).ok_or_else(|| {
                        KabiPayError::Validation("withholding basis component is missing".into())
                    })?;
                }
                Ok::<_, KabiPayError>(money(
                    basis * input.settings.percentage.unwrap_or(Decimal::ZERO),
                ))
            })
            .transpose()?
    } else {
        withholding.as_ref().map(|v| v.monthly)
    };
    let employment_count = months.len() as u32;
    for month in &mut months {
        if month.aggregate_source.is_some()
            || matches!(
                month.evidence,
                EvidenceKind::ImportedActual | EvidenceKind::FinalizedPayroll
            )
        {
            continue;
        }
        month.projected_withholding =
            if input.settings.method == WithholdingMethod::PercentageOverride {
                input
                    .settings
                    .basis_components
                    .iter()
                    .map(|code| month.components.get(code).copied())
                    .collect::<Option<Vec<_>>>()
                    .map(|values| {
                        money(
                            values.into_iter().sum::<Decimal>()
                                * input.settings.percentage.unwrap_or(Decimal::ZERO),
                        )
                    })
            } else if month.evidence == EvidenceKind::HistoricalEstimate {
                tax.as_ref()
                    .map(|value| money(value.statutory_tax / Decimal::from(employment_count)))
            } else {
                withholding.as_ref().map(|value| value.monthly)
            };
    }
    Ok(TaxProjection{fiscal_year:input.fiscal_year,configuration:Some(ProjectionConfiguration{regime:input.settings.regime,method:input.settings.method,percentage:input.settings.percentage,basis_components:input.settings.basis_components.clone(),effective_from:input.settings.effective_from}),months,annual_earnings,tax,withholding,recorded_tds,history_complete,selected_monthly_tds,limitations,opening_history:input.opening_history.clone(),
        note:"Estimated tax based on your current salary structure, joining date and available payroll information. Missing historical earnings are estimated. Actual deductions may change when HR updates your records. Contact HR for confirmation.".into()})
}
