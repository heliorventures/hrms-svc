use kabipay_loans_domain::*;
use rust_decimal::Decimal;
fn policy() -> LoanPolicy {
    LoanPolicy {
        eligibility: EligibilityPolicy {
            employment_types: vec!["FULL_TIME".into()],
            minimum_service_days: 0,
        },
        exposure: ExposurePolicy {
            per_loan: Decimal::from(10000),
            per_employee: Decimal::from(20000),
            per_company: Decimal::from(100000),
        },
        interest: InterestPolicy {
            methods: vec![InterestKind::InterestFree],
            maximum_rate: Decimal::ZERO,
            maximum_fixed_charge: Decimal::ZERO,
        },
        recovery: RecoveryPolicy {
            modes: vec![RecoveryMode::Payroll],
            priority: RecoveryPriority::OldestApprovedFirst,
            minimum_monthly_amount: Decimal::from(100),
            max_instalments: 24,
            allow_residual: false,
            allow_overrides: true,
            allow_skips: true,
            allow_accrual_pause: false,
            minimum_net_pay: Decimal::from(5000),
            maximum_net_pay_percentage: Decimal::from(30),
        },
        short_salary: ShortSalaryPolicy::CapAndCarry,
        allocation: AllocationOrder::InterestFirst,
        excess_credit: ExcessCreditPolicy::HoldForReview,
        early_settlement: EarlySettlementPolicy::KeepAssessedCharge,
        exit_recovery: ExitRecoveryPolicy {
            allow_fnf: true,
            allow_continuing: true,
            review_reference: "test-only-reviewed-arrangement".into(),
        },
        tax_treatment: TaxTreatmentPolicy {
            jurisdiction: "test-jurisdiction".into(),
            review_reference: "test-only-tax-review".into(),
            external_assessment_required: true,
        },
        approval: ApprovalPolicy {
            workflow_id: "10000000-0000-0000-0000-000000000001".into(),
            acknowledgement_required: false,
        },
    }
}
fn currency() -> Currency {
    Currency {
        code: "INR".into(),
        minor_units: 2,
    }
}
#[test]
fn policy_requires_explicit_limits_workflow_and_reviewed_tax_treatment() {
    let mut p = policy();
    assert!(p.validate(&currency()).is_ok());
    p.tax_treatment.review_reference.clear();
    assert!(p.validate(&currency()).is_err());
    p = policy();
    p.approval.workflow_id.clear();
    assert!(p.validate(&currency()).is_err());
    p = policy();
    p.exposure.per_company = Decimal::ZERO;
    assert!(p.validate(&currency()).is_err());
}
#[test]
fn approval_cannot_choose_unavailable_interest_or_bypass_tenure() {
    let p = policy();
    let mut t = LoanTerms {
        currency: currency(),
        approved_principal: Decimal::from(1000),
        interest: InterestMethod::InterestFree,
        rounding: RoundingRule::HalfUp,
        allocation: AllocationOrder::InterestFirst,
        recovery: RecoveryMode::Payroll,
        monthly_amount: Decimal::from(100),
        max_instalments: 24,
        allow_residual: false,
        calculator_version: CALCULATOR_VERSION.into(),
    };
    assert!(p.validate_terms(&t).is_ok());
    t.interest = InterestMethod::OneTimePercentage {
        rate: Decimal::from(5),
    };
    assert!(p.validate_terms(&t).is_err());
    t.interest = InterestMethod::InterestFree;
    t.max_instalments = 25;
    assert!(p.validate_terms(&t).is_err());
}
#[test]
fn policy_does_not_accept_binary_float_money_or_an_implicit_priority() {
    let mut value = serde_json::to_value(policy()).unwrap();
    value["exposure"]["perLoan"] = serde_json::json!(10000.01);
    assert!(serde_json::from_value::<LoanPolicy>(value).is_err());
    let mut value = serde_json::to_value(policy()).unwrap();
    value["recovery"]
        .as_object_mut()
        .unwrap()
        .remove("priority");
    assert!(serde_json::from_value::<LoanPolicy>(value).is_err());
}

#[test]
fn wire_decimals_cannot_silently_round_excess_precision() {
    let mut value = serde_json::to_value(policy()).unwrap();
    value["interest"]["maximumRate"] = serde_json::json!("0.12345678901234567890123456789");
    assert!(serde_json::from_value::<LoanPolicy>(value).is_err());
}
