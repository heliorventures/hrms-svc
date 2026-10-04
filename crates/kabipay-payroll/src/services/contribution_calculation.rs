use super::contribution_rules::*;
use chrono::{Datelike, NaiveDate};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_tax::domain::validate_amount;
use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::BTreeMap;

pub fn calculate_esi_wages(
    wages: &BTreeMap<String, Decimal>,
    classifications: &BTreeMap<String, WageClassification>,
) -> KabiPayResult<Decimal> {
    let mut included = Decimal::ZERO;
    let mut excluded = Decimal::ZERO;
    for (code, value) in wages {
        validate_amount(*value)?;
        if value.is_zero() {
            continue;
        }
        match classifications.get(code).ok_or_else(|| {
            KabiPayError::Validation(
                "every ESI wage component needs an explicit classification".into(),
            )
        })? {
            WageClassification::Included => included += *value,
            WageClassification::ExcludedWithAddback => excluded += *value,
            WageClassification::ExcludedOutsideAddback => {}
        }
    }
    Ok(included + (excluded - (included + excluded) / Decimal::from(2)).max(Decimal::ZERO))
}
pub fn calculate_esi(input: &EsiInput) -> KabiPayResult<EsiResult> {
    validate_amount(input.regular_wages)?;
    validate_amount(input.earned_wages)?;
    let zero = EsiResult {
        employee: Decimal::ZERO,
        employer: Decimal::ZERO,
        wage_basis: input.earned_wages,
    };
    if !input.company_covered {
        return Ok(zero);
    }
    let eligible = input.employee_eligible.ok_or_else(|| {
        KabiPayError::Validation("future employee ESI eligibility is not confirmed".into())
    })?;
    if !eligible {
        return Ok(zero);
    }
    let ceiling = Decimal::from(match input.disability {
        Some(true) => 25000,
        Some(false) => 21000,
        None if input.regular_wages <= Decimal::from(21000) => 21000,
        None => {
            return Err(KabiPayError::Validation(
                "ESI disability ceiling applicability needs review".into(),
            ))
        }
    });
    let period_end = if (4..=9).contains(&input.as_of.month()) {
        NaiveDate::from_ymd_opt(input.as_of.year(), 9, 30)
    } else {
        NaiveDate::from_ymd_opt(
            input.as_of.year() + i32::from(input.as_of.month() >= 10),
            3,
            31,
        )
    };
    if input.regular_wages > ceiling
        && (input.continuation_until != period_end
            || input.continuation_until.is_none_or(|v| v < input.as_of))
    {
        return Err(KabiPayError::Validation(
            "ESI wage ceiling exceeded; confirm contribution-period continuation".into(),
        ));
    }
    let daily = input.average_daily_wage.ok_or_else(|| {
        KabiPayError::Validation(
            "ESI average daily wage is required to establish employee-share exemption".into(),
        )
    })?;
    validate_amount(daily)?;
    Ok(EsiResult {
        employee: if daily <= Decimal::from(176) {
            Decimal::ZERO
        } else {
            (input.earned_wages * Decimal::new(75, 4)).ceil()
        },
        employer: (input.earned_wages * Decimal::new(325, 4)).ceil(),
        wage_basis: input.earned_wages,
    })
}
pub fn formula_basis(
    values: &BTreeMap<String, Decimal>,
    formula: &ComponentFormula,
) -> KabiPayResult<Decimal> {
    formula.validate()?;
    let mut result = Decimal::ZERO;
    for (code, weight) in &formula.weights {
        let amount = *values.get(code).ok_or_else(|| {
            KabiPayError::Validation("contribution basis component is missing".into())
        })?;
        validate_amount(amount)?;
        result += amount * weight;
    }
    Ok(formula.ceiling.map_or(result, |cap| result.min(cap)))
}
pub fn round_contribution(value: Decimal, rounding: &str) -> Decimal {
    match rounding {
        "CEIL_RUPEE" => value.ceil(),
        "HALF_UP_RUPEE" => value.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero),
        _ => value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
    }
}
pub fn calculate_contributions(
    input: &ContributionInput,
    policy: &ContributionPolicy,
) -> KabiPayResult<ContributionResult> {
    policy.validate()?;
    if input.as_of < policy.effective_from
        || policy.effective_until.is_some_and(|v| v < input.as_of)
    {
        return Err(KabiPayError::Validation(
            "contribution policy is not effective for this month".into(),
        ));
    }
    let pf = input.eligibility.pf_applicable.ok_or_else(|| {
        KabiPayError::Validation("employee PF applicability is not confirmed".into())
    })?;
    let pf_employee = if pf {
        round_contribution(
            formula_basis(&input.earned_components, &policy.pf_employee)? * policy.pf_employee.rate,
            &policy.pf_employee.rounding,
        )
    } else {
        Decimal::ZERO
    };
    let pf_employer = if pf {
        round_contribution(
            formula_basis(&input.earned_components, &policy.pf_employer)? * policy.pf_employer.rate,
            &policy.pf_employer.rounding,
        )
    } else {
        Decimal::ZERO
    };
    let esi = if policy.esi_mode == "INDIA_COSS_2025" {
        calculate_esi(&EsiInput {
            as_of: input.as_of,
            company_covered: policy.company_esi_covered,
            employee_eligible: input.eligibility.esi_applicable,
            regular_wages: calculate_esi_wages(&input.regular_components, &policy.classifications)?,
            earned_wages: calculate_esi_wages(&input.earned_components, &policy.classifications)?,
            continuation_until: input.eligibility.esi_continuation_until,
            disability: input.eligibility.disability,
            average_daily_wage: input.eligibility.average_daily_wage,
        })?
    } else {
        let eligible = if policy.company_esi_covered {
            input.eligibility.esi_applicable.ok_or_else(|| {
                KabiPayError::Validation("employee ESI applicability is not confirmed".into())
            })?
        } else {
            false
        };
        let basis = formula_basis(&input.earned_components, &policy.esi_basis)?;
        EsiResult {
            employee: if eligible {
                round_contribution(basis * policy.esi_basis.rate, &policy.esi_basis.rounding)
            } else {
                Decimal::ZERO
            },
            employer: if eligible {
                round_contribution(basis * policy.esi_employer_rate, &policy.esi_basis.rounding)
            } else {
                Decimal::ZERO
            },
            wage_basis: basis,
        }
    };
    let professional_tax = input
        .eligibility
        .professional_tax
        .or(policy.professional_tax)
        .ok_or_else(|| {
            KabiPayError::Validation(
                "professional tax requires HR configuration for future periods".into(),
            )
        })?;
    validate_amount(professional_tax)?;
    Ok(ContributionResult {
        pf_employee,
        pf_employer,
        esi_employee: esi.employee,
        esi_employer: esi.employer,
        professional_tax,
        esi_wage_basis: esi.wage_basis,
        origin: policy.origin.clone(),
        reason: policy.reason.clone(),
    })
}
