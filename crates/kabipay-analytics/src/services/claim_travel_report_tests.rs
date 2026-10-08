use super::super::claim_travel_report_filters::ClaimTravelFilter;
use super::super::hr_reports::{render_csv, ReportFilter};
use super::*;
use crate::resolvers::hr_report_types::ClaimTravelReportFilterInput;

fn base() -> ReportFilter {
    ReportFilter {
        from_date: "2026-10-01".parse().unwrap(),
        to_date: "2026-10-31".parse().unwrap(),
        employee_id: None,
        employee_search: Some("Asha_10%".into()),
    }
}

#[test]
fn claim_travel_filter_accepts_every_expense_payment_state() {
    for status in ["NONE", "PENDING_PAYMENT", "PAID", "FAILED", "ON_HOLD"] {
        let input = ClaimTravelReportFilterInput {
            payment_status: Some(format!(" {} ", status.to_lowercase())),
            ..Default::default()
        };
        let filter = ClaimTravelFilter::new(base(), Some(input), HrReportKind::ExpenseClaims)
            .unwrap_or_else(|error| panic!("supported payment status {status}: {error}"));
        assert_eq!(filter.payment_status.as_deref(), Some(status));
        assert!(ClaimTravelFilter::new(
            base(),
            Some(ClaimTravelReportFilterInput {
                payment_status: Some(status.into()),
                ..Default::default()
            }),
            HrReportKind::TravelRequests,
        )
        .is_err());
    }
}

#[test]
fn claim_travel_filter_rejects_incompatible_fields_and_invalid_statuses() {
    for (kind, input) in [
        (
            HrReportKind::TravelRequests,
            ClaimTravelReportFilterInput {
                payment_status: Some("PAID".into()),
                ..Default::default()
            },
        ),
        (
            HrReportKind::ExpenseClaims,
            ClaimTravelReportFilterInput {
                route_search: Some("Pune".into()),
                ..Default::default()
            },
        ),
        (
            HrReportKind::TravelRequests,
            ClaimTravelReportFilterInput {
                approval_status: Some("PARTIAL_APPROVED".into()),
                ..Default::default()
            },
        ),
        (
            HrReportKind::ExpenseClaims,
            ClaimTravelReportFilterInput {
                payment_status: Some("PENDING".into()),
                ..Default::default()
            },
        ),
        (
            HrReportKind::LeaveRequests,
            ClaimTravelReportFilterInput::default(),
        ),
    ] {
        assert!(ClaimTravelFilter::new(base(), Some(input), kind).is_err());
    }
    let normalized = ClaimTravelFilter::new(
        base(),
        Some(ClaimTravelReportFilterInput {
            approval_status: Some(" partially_approved ".into()),
            ..Default::default()
        }),
        HrReportKind::ExpenseClaims,
    );
    assert!(normalized.is_err());
    let normalized = ClaimTravelFilter::new(
        base(),
        Some(ClaimTravelReportFilterInput {
            approval_status: Some(" partial_approved ".into()),
            ..Default::default()
        }),
        HrReportKind::ExpenseClaims,
    )
    .unwrap();
    assert_eq!(
        normalized.approval_status.as_deref(),
        Some("PARTIAL_APPROVED")
    );
}

#[test]
fn claim_travel_search_is_literal_and_bound() {
    let filter = ClaimTravelFilter::new(base(), None, HrReportKind::ExpenseClaims).unwrap();
    assert_eq!(
        literal_substring(filter.base.employee_search.as_deref()).as_deref(),
        Some("%Asha\\_10\\%%")
    );
    let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
    let query = statement(
        rows_sql(&source(HrReportKind::ExpenseClaims).unwrap(), 50, 100),
        Uuid::nil(),
        &filter,
        &clock,
    );
    assert!(query.sql.contains("LIMIT 50 OFFSET 100"));
    assert!(!query.sql.contains("Asha"));
    assert!(query.sql.ends_with("ORDER BY sort_order"));
    assert!(query.sql.contains("r.tenant_id=$1"));
}

#[test]
fn claim_travel_date_rules_and_nullable_amounts_are_preserved() {
    let expense = source(HrReportKind::ExpenseClaims).unwrap();
    assert!(expense
        .predicate
        .contains("r.expense_date BETWEEN $2 AND $3"));
    assert!(expense
        .fields
        .contains("r.amount::text,r.approved_amount::text,r.currency"));
    assert!(expense.columns.contains(&"Payment status"));
    let travel = source(HrReportKind::TravelRequests).unwrap();
    assert!(travel
        .predicate
        .contains("r.from_date<=$3 AND r.to_date>=$2"));
    assert!(travel.fields.contains("r.estimated_amount::text"));
}

#[test]
fn claim_travel_csv_keeps_decimal_null_and_formula_safety() {
    let columns = vec!["Claimed".into(), "Approved".into(), "Title".into()];
    let rows = vec![vec![
        "12345678901234567890.01".into(),
        "".into(),
        "=SUM(1,2)\n\"client\"".into(),
    ]];
    let csv = render_csv(&columns, &rows);
    assert!(csv.contains("\"12345678901234567890.01\",\"\""));
    assert!(csv.contains("\"'=SUM(1,2)\n\"\"client\"\"\""));
    assert!(validate_export_count(10_000).is_ok());
    assert!(
        matches!(validate_export_count(10_001), Err(KabiPayError::Validation(message)) if message.contains("Narrow the filters"))
    );
}
