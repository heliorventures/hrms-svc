use chrono::NaiveDate;
use kabipay_tax::domain::{
    projection::{project_months, ActualMonth, SalaryPeriod},
    tax_year::employment_months,
    EvidenceKind,
};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
fn d(v: &str) -> NaiveDate {
    v.parse().unwrap()
}
#[test]
fn finalized_selected_month_is_counted_once_and_never_reprojected() {
    use kabipay_tax::domain::{calculate_projection, TaxProjectionInput};
    let mut input:TaxProjectionInput=serde_json::from_value(serde_json::json!({
        "fiscal_year":2026,"joining":"2025-01-01","exit":null,"as_of":"2026-09-01",
        "salaries":[{"from":"2025-01-01","until":null,"components":{"BASIC":"200000.00"},"divisor":31}],"actuals":[],
        "settings":{"regime":"NEW","method":"ANNUAL_PROJECTION","percentage":null,"basis_components":[],"effective_from":"2026-04-01","effective_until":null,"reason":null},
        "approved_deductions":"0.00","resident":true,"age_at_year_end":40,"previous_employer_earnings":"0.00","previous_employer_tds":null,"taxable_component_codes":["BASIC"],"opening_history":[]
    })).unwrap();
    input.actuals = (4..=9)
        .map(|month| ActualMonth {
            year: 2026,
            month,
            components: BTreeMap::from([("BASIC".into(), Decimal::from(200000))]),
            tds: Some(Decimal::from(10000)),
            evidence: EvidenceKind::FinalizedPayroll,
        })
        .collect();
    let result = calculate_projection(&input).unwrap();
    assert_eq!(result.recorded_tds, Decimal::from(60000));
    assert_eq!(result.selected_monthly_tds, Some(Decimal::from(10000)));
    assert_eq!(serde_json::to_value(&result).unwrap()["selected_month"], serde_json::json!({"year":2026,"month":9,"evidence":"FINALIZED_PAYROLL"}));
    assert_eq!(result.withholding.unwrap().monthly, Decimal::from(38750));
}
#[test]
fn financial_year_never_extends_to_next_april() {
    let months = employment_months(2026, d("2026-05-01"), None).unwrap();
    assert_eq!(months.len(), 11);
    assert_eq!(months.last().unwrap().end, d("2027-03-31"));
    assert_eq!(
        employment_months(2026, d("2025-05-01"), None)
            .unwrap()
            .len(),
        12
    );
    assert_eq!(
        employment_months(2023, d("2024-02-01"), Some(d("2024-02-29"))).unwrap()[0].days,
        29
    );
}
#[test]
fn salary_revision_midmonth_join_and_actual_replacement() {
    let salaries = vec![
        SalaryPeriod {
            from: d("2026-05-16"),
            until: None,
            components: BTreeMap::from([("BASIC".into(), Decimal::from(31000))]),
            divisor: Some(31),
        },
        SalaryPeriod {
            from: d("2026-10-01"),
            until: None,
            components: BTreeMap::from([("BASIC".into(), Decimal::from(62000))]),
            divisor: Some(31),
        },
    ];
    let actual = vec![ActualMonth {
        year: 2026,
        month: 9,
        components: BTreeMap::from([("BASIC".into(), Decimal::from(25000))]),
        tds: Some(Decimal::from(2500)),
        evidence: EvidenceKind::ImportedActual,
    }];
    let rows = project_months(
        2026,
        d("2026-05-16"),
        None,
        d("2026-10-01"),
        &salaries,
        &actual,
    )
    .unwrap();
    assert_eq!(rows.len(), 11);
    assert_eq!(rows[0].earnings, Decimal::from(16000));
    assert_eq!(rows[4].earnings, Decimal::from(25000));
    assert_eq!(rows[4].evidence, EvidenceKind::ImportedActual);
    assert_eq!(rows[5].earnings, Decimal::from(62000));
    assert_eq!(rows[0].tds, None);
}

#[test]
fn projection_includes_taxable_incentive_but_percentage_basis_can_exclude_it() {
    use kabipay_tax::domain::{calculate_projection, TaxProjectionInput};
    let input:TaxProjectionInput=serde_json::from_value(serde_json::json!({
        "fiscal_year":2026,"joining":"2026-05-01","exit":null,"as_of":"2026-10-01",
        "salaries":[{"from":"2026-05-01","until":null,"components":{"BASIC":"100000.00"},"divisor":31}],
        "actuals":[{"year":2026,"month":10,"components":{"BASIC":"100000.00","INCENTIVE":"10000.00"},"tds":null,"evidence":"FUTURE_PROJECTION"}],
        "settings":{"regime":"NEW","method":"PERCENTAGE_OVERRIDE","percentage":"0.10","basis_components":["BASIC"],"effective_from":"2026-10-01","effective_until":null,"reason":"HR instruction","resident":true},
        "approved_deductions":"0.00","resident":true,"age_at_year_end":40,
        "previous_employer_earnings":"0.00","previous_employer_tds":null,
        "taxable_component_codes":["BASIC","INCENTIVE"],"opening_history":[]
    })).unwrap();
    let result = calculate_projection(&input).unwrap();
    assert_eq!(result.annual_earnings, Decimal::from(1110000));
    assert_eq!(result.selected_monthly_tds, Some(Decimal::from(10000)));
    assert!(!result.history_complete);
    assert_eq!(result.withholding.unwrap().remaining, None);
}

#[test]
fn aggregate_opening_replaces_covered_estimates_without_fabricating_monthly_tds() {
    use kabipay_tax::domain::{calculate_projection, TaxProjectionInput};
    let mut input:TaxProjectionInput=serde_json::from_value(serde_json::json!({
        "fiscal_year":2026,"joining":"2025-05-01","exit":null,"as_of":"2026-09-01",
        "salaries":[{"from":"2026-09-01","until":null,"components":{"BASIC":"100000.00"},"divisor":31}],"actuals":[],
        "settings":{"regime":"NEW","method":"ANNUAL_PROJECTION","percentage":null,"basis_components":[],"effective_from":"2026-09-01","effective_until":null,"reason":null},
        "approved_deductions":"0.00","resident":true,"age_at_year_end":40,"previous_employer_earnings":"0.00","previous_employer_tds":null,
        "taxable_component_codes":["BASIC"],
        "opening_history":[{"fiscal_year":2026,"period_start":"2026-04-01","period_end":"2026-08-31","employer":"CURRENT","source_key":"opening","earnings":"400000.00","components":{"BASIC":"400000.00"},"tds":"10000.00","coverage":"COMPLETE","reason":"HR supplied history","evidence":"IMPORTED_ACTUAL"}]
    })).unwrap();
    let result = calculate_projection(&input).unwrap();
    assert_eq!(result.annual_earnings, Decimal::from(1100000));
    assert_eq!(result.recorded_tds, Decimal::from(10000));
    assert!(result.history_complete);
    assert!(result.months[0].tds.is_none());
    assert!(result.months[0].components.is_empty());
    input.previous_employer_earnings = Decimal::from(10000);
    input.previous_employer_tds = Some(Decimal::from(1000));
    let partial = calculate_projection(&input).unwrap();
    assert!(
        !partial.history_complete,
        "a known partial deduction does not establish complete history"
    );
    assert_eq!(partial.recorded_tds, Decimal::from(11000));
    assert!(partial.withholding.unwrap().remaining.is_none());
    input.previous_employer_history_complete = Some(true);
    assert!(calculate_projection(&input).unwrap().history_complete);
    input.previous_employer_earnings = Decimal::ZERO;
    input.previous_employer_tds = None;
    input.previous_employer_history_complete = Some(false);
    assert!(!calculate_projection(&input).unwrap().history_complete);
}

#[test]
fn payroll_month_does_not_require_unrelated_historical_assignments() {
    use kabipay_tax::domain::projection::project_payroll_month;
    let salaries = vec![SalaryPeriod {
        from: d("2026-10-01"),
        until: None,
        components: BTreeMap::from([("BASIC".into(), Decimal::from(31000))]),
        divisor: Some(31),
    }];
    let result = project_payroll_month(2026, 10, d("2025-01-01"), None, &salaries).unwrap();
    assert_eq!(result.earnings, Decimal::from(31000));
    let exited =
        project_payroll_month(2026, 10, d("2025-01-01"), Some(d("2026-10-15")), &salaries).unwrap();
    assert_eq!(exited.earnings, Decimal::from(15000));
}
