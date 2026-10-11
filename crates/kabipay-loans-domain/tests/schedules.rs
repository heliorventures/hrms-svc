use chrono::NaiveDate;
use kabipay_loans_domain::*;
use rust_decimal::Decimal;
fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}
fn input() -> ScheduleInput {
    ScheduleInput {
        terms: LoanTerms {
            currency: Currency {
                code: "INR".into(),
                minor_units: 2,
            },
            approved_principal: Decimal::from(60000),
            interest: InterestMethod::InterestFree,
            rounding: RoundingRule::HalfUp,
            allocation: AllocationOrder::InterestFirst,
            recovery: RecoveryMode::Payroll,
            monthly_amount: Decimal::from(5000),
            max_instalments: 24,
            allow_residual: false,
            calculator_version: CALCULATOR_VERSION.into(),
        },
        principal: Decimal::from(60000),
        due_interest: Decimal::ZERO,
        carry: Decimal::ZERO,
        accrual_start: date("2026-10-01"),
        first_due_date: date("2026-11-01"),
        overrides: vec![],
    }
}
#[test]
fn interest_free_schedule_has_twelve_payments_and_exact_zero_balance() {
    let s = project_schedule(&input()).unwrap();
    assert_eq!(s.items.len(), 12);
    assert_eq!(s.items[0].total, Decimal::from(5000));
    assert_eq!(s.residual_principal, Decimal::ZERO);
}
#[test]
fn explicit_skip_keeps_principal_and_extends_schedule() {
    let mut i = input();
    i.overrides.push(PeriodOverride {
        period: date("2026-11-01"),
        amount: None,
        pause_interest: false,
    });
    let s = project_schedule(&i).unwrap();
    assert_eq!(s.items.len(), 13);
    assert_eq!(s.items[0].total, Decimal::ZERO);
    assert_eq!(s.items[0].principal_after, Decimal::from(60000));
}
#[test]
fn final_instalment_is_capped_at_payoff() {
    let mut i = input();
    i.principal = Decimal::from(7500);
    let s = project_schedule(&i).unwrap();
    assert_eq!(s.items.len(), 2);
    assert_eq!(s.items[1].total, Decimal::from(2500));
}
#[test]
fn tenure_residual_requires_an_explicit_exception() {
    let mut i = input();
    i.terms.max_instalments = 2;
    assert_eq!(
        project_schedule(&i).unwrap_err(),
        LoanDomainError::NonAmortizing
    );
    i.terms.allow_residual = true;
    assert_eq!(
        project_schedule(&i).unwrap().residual_principal,
        Decimal::from(50000)
    );
}
#[test]
fn lifecycle_does_not_allow_withdrawal_or_reapproval_after_approval() {
    assert_eq!(
        transition_request(RequestState::Submitted, RequestAction::Approve).unwrap(),
        RequestState::Approved
    );
    assert!(transition_request(RequestState::Approved, RequestAction::Withdraw).is_err());
    assert!(transition_request(RequestState::Rejected, RequestAction::Approve).is_err());
    assert_eq!(
        transition_request(RequestState::Returned, RequestAction::Submit).unwrap(),
        RequestState::Submitted
    );
}
#[test]
fn monthly_schedule_uses_selected_convention_and_interest_first() {
    let mut i = input();
    i.principal = Decimal::from(12000);
    i.terms.approved_principal = i.principal;
    i.terms.monthly_amount = Decimal::from(1100);
    i.terms.interest = InterestMethod::ReducingBalance {
        annual_rate: Decimal::from(12),
        convention: InterestConvention::Monthly {
            partial_period: PartialPeriodTreatment::Reject,
        },
    };
    let s = project_schedule(&i).unwrap();
    assert_eq!(s.items[0].interest, Decimal::from(120));
    assert_eq!(s.items[0].principal_after, Decimal::from(11020));
}
#[test]
fn one_time_charge_is_not_reassessed_each_month() {
    let mut i = input();
    i.principal = Decimal::from(6000);
    i.due_interest = Decimal::from(600);
    i.terms.interest = InterestMethod::OneTimePercentage {
        rate: Decimal::from(10),
    };
    let s = project_schedule(&i).unwrap();
    assert_eq!(
        s.items.iter().map(|r| r.interest).sum::<Decimal>(),
        Decimal::from(600)
    );
}

#[test]
fn proportional_charge_carry_is_reserved_for_funding_not_monthly_accrual() {
    let mut i = input();
    i.principal = Decimal::from(1000);
    i.due_interest = Decimal::from(1);
    i.carry = Decimal::new(1, 3);
    i.terms.interest = InterestMethod::OneTimeFixed {
        amount: Decimal::from(10),
        approved_principal: Decimal::from(60000),
    };
    let s = project_schedule(&i).unwrap();
    assert_eq!(s.carry, i.carry);
}

#[test]
fn month_end_due_dates_preserve_the_original_day_anchor() {
    let mut i = input();
    i.first_due_date = date("2027-01-31");
    let s = project_schedule(&i).unwrap();
    assert_eq!(s.items[1].due_date, date("2027-02-28"));
    assert_eq!(s.items[2].due_date, date("2027-03-31"));
}

#[test]
fn paused_calendar_month_only_excludes_that_month_from_accrual() {
    let mut i = input();
    i.accrual_start = date("2027-01-15");
    i.first_due_date = date("2027-03-15");
    i.principal = Decimal::from(10000);
    i.terms.interest = InterestMethod::ReducingBalance {
        annual_rate: Decimal::from(12),
        convention: InterestConvention::Act365Fixed,
    };
    i.overrides.push(PeriodOverride {
        period: date("2027-02-01"),
        amount: None,
        pause_interest: true,
    });
    let schedule = project_schedule(&i).unwrap();
    // January 15-February 1 (17 days) and March 1-15 (14 days).
    assert_eq!(schedule.items[0].interest, Decimal::new(10192, 2));
}

#[test]
fn pause_in_due_month_preserves_interest_earned_before_that_month() {
    let mut i = input();
    i.accrual_start = date("2027-01-15");
    i.first_due_date = date("2027-02-15");
    i.principal = Decimal::from(10000);
    i.terms.interest = InterestMethod::ReducingBalance {
        annual_rate: Decimal::from(12),
        convention: InterestConvention::Act365Fixed,
    };
    i.overrides.push(PeriodOverride {
        period: date("2027-02-01"),
        amount: Some(Decimal::from(5000)),
        pause_interest: true,
    });
    let schedule = project_schedule(&i).unwrap();
    assert_eq!(schedule.items[0].interest, Decimal::new(5589, 2));
}

#[test]
fn calendar_pause_preserves_rounding_carry_until_next_active_interval() {
    let i = AccrualInput {
        principal: Decimal::from(10000),
        method: InterestMethod::ReducingBalance {
            annual_rate: Decimal::from(12),
            convention: InterestConvention::Act365Fixed,
        },
        start: date("2027-01-15"),
        end_exclusive: date("2027-03-15"),
        carry: Decimal::new(3, 3),
        currency: input().terms.currency,
        rounding: RoundingRule::HalfUp,
    };
    let result = accrue_calendar_interest(&i, &[date("2027-02-01")]).unwrap();
    let january = accrue_interest(&AccrualInput {
        end_exclusive: date("2027-02-01"),
        ..i.clone()
    })
    .unwrap();
    let march = accrue_interest(&AccrualInput {
        start: date("2027-03-01"),
        carry: january.carry,
        ..i
    })
    .unwrap();
    assert_eq!(
        result.posted_interest,
        january.posted_interest + march.posted_interest
    );
    assert_eq!(result.carry, march.carry);
    assert!(result.intervals[1].paused);
    assert_eq!(result.intervals[1].posted, Decimal::ZERO);
}
