use kabipay_payroll::services::contribution_rules::ContributionPolicy;
#[test]
fn custom_source_formula_requires_reason_and_effective_period() {
    let value = serde_json::json!({"effective_from":"2026-10-01","effective_until":null,"lwp_divisor":31,
        "origin":"IMPORTED_CLIENT_RULE","reason":"","pf_employee":{"weights":{"BASIC":"1"},"rate":"0.12","ceiling":null,"rounding":"HALF_UP_2DP"},
        "pf_employer":{"weights":{"BASIC":"1"},"rate":"0.13","ceiling":null,"rounding":"HALF_UP_2DP"},
        "esi_basis":{"weights":{"BASIC":"1"},"rate":"0.0075","ceiling":null,"rounding":"CEIL_RUPEE"},
        "esi_employer_rate":"0.0325","company_esi_covered":true,"esi_mode":"CUSTOM_COMPONENTS","classifications":{},"professional_tax":"200.00"});
    let policy: ContributionPolicy = serde_json::from_value(value.clone()).unwrap();
    assert!(policy.validate().is_err());
    let mut approved = value;
    approved["reason"] = serde_json::json!("Reviewed workbook basis; HR may revise prospectively");
    assert!(serde_json::from_value::<ContributionPolicy>(approved)
        .unwrap()
        .validate()
        .is_ok());
}

fn automatic_input() -> kabipay_payroll::services::automatic_payroll::EmployeePayrollInput {
    use rust_decimal::Decimal;
    use std::collections::BTreeMap;
    let formula = serde_json::json!({"weights":{"BASIC":"1"},"rate":"0.12","ceiling":null,"rounding":"HALF_UP_2DP"});
    let policy=serde_json::from_value(serde_json::json!({"effective_from":"2026-10-01","effective_until":null,"lwp_divisor":31,
        "origin":"HR_CONFIGURATION","reason":"Reviewed formula","pf_employee":formula,"pf_employer":formula,
        "esi_basis":formula,"esi_employer_rate":"0.0325","company_esi_covered":false,"esi_mode":"CUSTOM_COMPONENTS","classifications":{},"professional_tax":"200.00"})).unwrap();
    let period=serde_json::from_value(serde_json::json!({"year":2026,"month":10,"gross_rule":"FIXED_MINUS_LWP","fixed_gross":"31000",
        "lwp_days":"1","lwp_divisor":"31","lwp_basis":"GROSS","lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION",
        "variable_allowance_ot":"0","incentive":"1000","advance_already_paid":"5000","additional_deductions":[],
        "statutory_overrides":{},"expected_earned_components":{},"expected_employer_contributions":{},"expected_statement":{},
        "historical_lwp_included":false,"ready":true,
        "automatic":{"eligibility":{"pf_applicable":false,"esi_applicable":false,"esi_continuation_until":null,"disability":false,"average_daily_wage":null},"withholding_override":null}})).unwrap();
    let projection=serde_json::from_value(serde_json::json!({"fiscal_year":2026,"joining":"2026-04-01","exit":null,"as_of":"2026-10-01",
        "salaries":[{"from":"2026-04-01","until":null,"components":{"BASIC":"31000.00"},"divisor":31}],"actuals":[],
        "settings":{"regime":"NEW","method":"PERCENTAGE_OVERRIDE","percentage":"0.10","basis_components":["BASIC"],"effective_from":"2026-10-01","effective_until":null,"reason":"Reviewed rate"},
        "approved_deductions":"0.00","resident":true,"age_at_year_end":40,"previous_employer_earnings":"0.00","previous_employer_tds":null,
        "taxable_component_codes":["BASIC","INCENTIVE"],"opening_history":[]})).unwrap();
    let components = BTreeMap::from([("BASIC".into(), Decimal::from(31000))]);
    kabipay_payroll::services::automatic_payroll::EmployeePayrollInput {
        period,
        regular_components: components.clone(),
        month_components: components,
        policy,
        projection,
        employer_pf_rule: None,
    }
}
#[test]
fn automatic_lwp_tax_basis_and_advance_settlement_reconcile() {
    use kabipay_payroll::services::automatic_payroll::calculate_employee_payroll;
    use rust_decimal::Decimal;
    let result = calculate_employee_payroll(&automatic_input()).unwrap();
    assert_eq!(result.calculation.gross, Decimal::from(30000));
    assert_eq!(result.calculation.statutory["TDS"], "3000.00");
    assert_eq!(result.calculation.total_deductions, Decimal::from(3200));
    assert_eq!(result.calculation.net_earned, Decimal::from(27800));
    assert_eq!(result.calculation.remaining_payable, Decimal::from(22800));
    assert!(result.requires_tax_acknowledgement);
}

#[test]
fn fixed_employer_pf_is_unchanged_for_partial_service_and_never_taxable() {
    use kabipay_payroll::services::automatic_payroll::calculate_employee_payroll;
    use rust_decimal::Decimal;
    let mut input = automatic_input();
    input.period.automatic.as_mut().unwrap().eligibility.pf_applicable = Some(true);
    input.employer_pf_rule = Some(serde_json::from_value(serde_json::json!({"fixed_monthly_amount":"3000.00","basis_components":[],"rate":"0","ceiling":null,"rounding":"HALF_UP_2DP","origin":"REVIEWED_CONFIGURATION"})).unwrap());
    for earnings in [31000, 16000] {
        input.month_components.insert("BASIC".into(), Decimal::from(earnings));
        let result = calculate_employee_payroll(&input).unwrap();
        assert_eq!(result.calculation.employer["pf"], "3000.00");
        assert!(!result.calculation.components.contains_key("EMPLOYER_PF"));
        assert_eq!(result.calculation.statutory["TDS"].parse::<Decimal>().unwrap(), result.calculation.gross / Decimal::TEN);
    }
    input.period.automatic.as_mut().unwrap().eligibility.pf_applicable = Some(false);
    assert_eq!(calculate_employee_payroll(&input).unwrap().calculation.employer["pf"], "0.00");
}

#[test]
fn recurring_employee_pt_overrides_company_default_including_zero() {
    use kabipay_payroll::services::automatic_payroll::calculate_employee_payroll;
    let mut input = automatic_input();
    let mut value =
        serde_json::to_value(&input.period.automatic.as_ref().unwrap().eligibility).unwrap();
    value["professional_tax"] = serde_json::json!("0");
    input.period.automatic.as_mut().unwrap().eligibility =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        calculate_employee_payroll(&input)
            .unwrap()
            .calculation
            .statutory["PT"],
        "0.00"
    );
    value["professional_tax"] = serde_json::json!("150");
    input.period.automatic.as_mut().unwrap().eligibility =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        calculate_employee_payroll(&input)
            .unwrap()
            .calculation
            .statutory["PT"],
        "150.00"
    );
    value["professional_tax"] = serde_json::json!("-1");
    input.period.automatic.as_mut().unwrap().eligibility = serde_json::from_value(value).unwrap();
    assert!(calculate_employee_payroll(&input).is_err());
}
#[test]
fn invalid_divisor_and_historical_usage_are_rejected_before_calculation() {
    use kabipay_payroll::services::automatic_payroll::calculate_employee_payroll;
    let mut invalid = automatic_input();
    invalid.policy.lwp_divisor = 0;
    assert!(calculate_employee_payroll(&invalid).is_err());
    let mut invalid = automatic_input();
    invalid.period.historical_lwp_included = true;
    assert!(calculate_employee_payroll(&invalid).is_err());
}

#[test]
fn configured_incentive_is_included_in_contribution_wages() {
    use kabipay_payroll::services::automatic_payroll::calculate_employee_payroll;
    use rust_decimal::Decimal;
    let mut input = automatic_input();
    input
        .policy
        .pf_employee
        .weights
        .insert("INCENTIVE".into(), Decimal::ONE);
    input
        .period
        .automatic
        .as_mut()
        .unwrap()
        .eligibility
        .pf_applicable = Some(true);
    let result = calculate_employee_payroll(&input).unwrap();
    // Earned BASIC 30,000 plus the separately paid incentive 1,000.
    assert_eq!(result.calculation.statutory["PF"], "3720.00");
    assert_eq!(result.calculation.gross, Decimal::from(30000));
    assert_eq!(result.calculation.incentive, Decimal::from(1000));
}

#[test]
fn included_incentive_uses_the_full_earned_esi_basis() {
    use kabipay_payroll::services::{
        automatic_payroll::calculate_employee_payroll, contribution_rules::WageClassification,
    };
    use rust_decimal::Decimal;
    let mut input = automatic_input();
    input
        .regular_components
        .insert("BASIC".into(), Decimal::from(10000));
    input.month_components = input.regular_components.clone();
    input.period.lwp_days = Some("0".into());
    input.period.incentive = Some("5000".into());
    input.policy.company_esi_covered = true;
    input.policy.esi_mode = "INDIA_COSS_2025".into();
    input
        .policy
        .classifications
        .insert("BASIC".into(), WageClassification::Included);
    input
        .policy
        .classifications
        .insert("INCENTIVE".into(), WageClassification::Included);
    let eligibility = &mut input.period.automatic.as_mut().unwrap().eligibility;
    eligibility.esi_applicable = Some(true);
    eligibility.average_daily_wage = Some(Decimal::from(500));
    let evidence = calculate_employee_payroll(&input)
        .unwrap()
        .contribution_evidence
        .unwrap();
    assert_eq!(evidence.esi_wage_basis, Decimal::from(15000));
    assert_eq!(evidence.esi_employee, Decimal::from(113));
    assert_eq!(evidence.esi_employer, Decimal::from(488));
}

#[test]
fn excess_recorded_tax_requires_an_explicit_reasoned_resolution() {
    use kabipay_payroll::services::automatic_payroll::{
        calculate_employee_payroll, WithholdingOverride,
    };
    use kabipay_tax::domain::{projection::ActualMonth, EvidenceKind, WithholdingMethod};
    use rust_decimal::Decimal;
    let mut input = automatic_input();
    input.projection.settings.method = WithholdingMethod::AnnualProjection;
    input.projection.settings.percentage = None;
    input.projection.settings.basis_components.clear();
    input.projection.actuals = (4..10)
        .map(|month| ActualMonth {
            year: 2026,
            month,
            components: input.regular_components.clone(),
            tds: Some(Decimal::from(10000)),
            evidence: EvidenceKind::FinalizedPayroll,
        })
        .collect();
    let error = calculate_employee_payroll(&input).unwrap_err();
    assert!(error.to_string().contains("exceeds projected annual tax"));
    input
        .period
        .automatic
        .as_mut()
        .unwrap()
        .withholding_override = Some(WithholdingOverride {
        amount: Decimal::ZERO,
        reason: "HR reviewed excess deductions; no refund through payroll".into(),
    });
    let reviewed = calculate_employee_payroll(&input).unwrap();
    assert!(
        reviewed
            .tax_projection
            .unwrap()
            .withholding
            .unwrap()
            .excess_withholding
    );
}
