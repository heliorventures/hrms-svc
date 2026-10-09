use chrono::NaiveDate;
use kabipay_loans_domain::*;
use rust_decimal::Decimal;
use std::str::FromStr;

fn d(value: &str) -> Decimal {
    Decimal::from_str(value).unwrap()
}
fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}
fn currency() -> Currency {
    Currency {
        code: "INR".into(),
        minor_units: 2,
    }
}
fn input(method: InterestMethod) -> AccrualInput {
    AccrualInput {
        principal: d("10000"),
        method,
        start: date("2026-01-01"),
        end_exclusive: date("2026-01-31"),
        carry: Decimal::ZERO,
        currency: currency(),
        rounding: RoundingRule::HalfUp,
    }
}
#[test]
fn daily_interest_and_allocation_reconcile_exactly() {
    let result = accrue_interest(&input(InterestMethod::ReducingBalance {
        annual_rate: d("12"),
        convention: InterestConvention::Act365Fixed,
    }))
    .unwrap();
    assert_eq!(result.posted_interest, d("98.63"));
    let allocation = allocate_repayment(&AllocationInput {
        payment: d("1000"),
        principal: d("10000"),
        interest: result.posted_interest,
        order: AllocationOrder::InterestFirst,
        currency: currency(),
    })
    .unwrap();
    assert_eq!(allocation.interest, d("98.63"));
    assert_eq!(allocation.principal, d("901.37"));
    assert_eq!(d("10000") - allocation.principal, d("9098.63"));
}
#[test]
fn one_time_percentage_applies_only_to_the_actual_tranche() {
    let mut tranche = input(InterestMethod::OneTimePercentage { rate: d("10") });
    tranche.principal = d("6000");
    assert_eq!(accrue_interest(&tranche).unwrap().posted_interest, d("600"));
}
#[test]
fn monthly_convention_has_explicit_full_calendar_periods() {
    let mut period = input(InterestMethod::ReducingBalance {
        annual_rate: d("12"),
        convention: InterestConvention::Monthly {
            partial_period: PartialPeriodTreatment::Reject,
        },
    });
    period.principal = d("12000");
    period.start = date("2026-11-01");
    period.end_exclusive = date("2026-12-01");
    let result = accrue_interest(&period).unwrap();
    assert_eq!(result.posted_interest, d("120"));
    let a = allocate_repayment(&AllocationInput {
        payment: d("1100"),
        principal: period.principal,
        interest: result.posted_interest,
        order: AllocationOrder::InterestFirst,
        currency: currency(),
    })
    .unwrap();
    assert_eq!(period.principal - a.principal, d("11020"));
    period.principal = d("11020");
    period.start = date("2026-12-01");
    period.end_exclusive = date("2027-01-01");
    assert_eq!(
        accrue_interest(&period).unwrap().posted_interest,
        d("110.20")
    );
}
#[test]
fn monthly_partial_period_requires_selected_treatment() {
    let mut p = input(InterestMethod::ReducingBalance {
        annual_rate: d("12"),
        convention: InterestConvention::Monthly {
            partial_period: PartialPeriodTreatment::Reject,
        },
    });
    p.start = date("2026-11-16");
    p.end_exclusive = date("2026-12-01");
    assert!(accrue_interest(&p).is_err());
    p.method = InterestMethod::ReducingBalance {
        annual_rate: d("12"),
        convention: InterestConvention::Monthly {
            partial_period: PartialPeriodTreatment::ProrateActualDays,
        },
    };
    assert_eq!(accrue_interest(&p).unwrap().posted_interest, d("50"));
}
#[test]
fn interest_free_and_excess_receipt_never_create_negative_debt() {
    assert_eq!(
        accrue_interest(&input(InterestMethod::InterestFree))
            .unwrap()
            .posted_interest,
        Decimal::ZERO
    );
    let a = allocate_repayment(&AllocationInput {
        payment: d("1100"),
        principal: d("1000"),
        interest: d("25"),
        order: AllocationOrder::InterestFirst,
        currency: currency(),
    })
    .unwrap();
    assert_eq!(a.principal, d("1000"));
    assert_eq!(a.interest, d("25"));
    assert_eq!(a.excess_credit, d("75"));
}
#[test]
fn carry_prevents_loss_from_daily_rounding_and_uses_fixed_365_in_leap_year() {
    let mut p = input(InterestMethod::ReducingBalance {
        annual_rate: d("12"),
        convention: InterestConvention::Act365Fixed,
    });
    p.principal = d("1");
    p.start = date("2024-02-28");
    p.end_exclusive = date("2024-03-01");
    let r = accrue_interest(&p).unwrap();
    assert_eq!(r.posted_interest, Decimal::ZERO);
    assert!(r.carry > Decimal::ZERO);
    p.start = date("2024-03-01");
    p.end_exclusive = date("2024-04-01");
    p.carry = r.carry;
    let r = accrue_interest(&p).unwrap();
    assert_eq!(r.posted_interest, d("0.01"));
}
#[test]
fn reject_invalid_currency_precision_negative_balance_and_reversed_dates() {
    let mut p = input(InterestMethod::InterestFree);
    p.currency.code = "inr".into();
    assert!(accrue_interest(&p).is_err());
    p.currency = currency();
    p.principal = d("-1");
    assert!(accrue_interest(&p).is_err());
    p.principal = d("1.001");
    assert!(accrue_interest(&p).is_err());
    p.principal = d("1");
    p.end_exclusive = date("2025-01-01");
    assert!(accrue_interest(&p).is_err());
}
