use kabipay_common::{
    expense_payment::normalize_expense_payment_status_wire, KabiPayError, KabiPayResult,
};
use uuid::Uuid;

use super::hr_reports::ReportFilter;
use crate::resolvers::hr_report_types::{ClaimTravelReportFilterInput, HrReportKind};

pub struct ClaimTravelFilter {
    pub base: ReportFilter,
    pub department_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
    pub expense_category_id: Option<Uuid>,
    pub approval_status: Option<String>,
    pub payment_status: Option<String>,
    pub route_search: Option<String>,
}

pub fn is_claim_travel(kind: HrReportKind) -> bool {
    matches!(
        kind,
        HrReportKind::ExpenseClaims | HrReportKind::TravelRequests
    )
}

impl ClaimTravelFilter {
    pub fn new(
        base: ReportFilter,
        input: Option<ClaimTravelReportFilterInput>,
        kind: HrReportKind,
    ) -> KabiPayResult<Self> {
        base.validate()?;
        if !is_claim_travel(kind) && input.is_some() {
            return Err(KabiPayError::Validation(
                "claimTravelFilter is only available for expense and travel reports".into(),
            ));
        }
        let input = input.unwrap_or_default();
        let mut filter = Self {
            base,
            department_id: parse_id(input.department_id, "departmentId")?,
            location_id: parse_id(input.location_id, "locationId")?,
            expense_category_id: parse_id(input.expense_category_id, "expenseCategoryId")?,
            approval_status: normalize(input.approval_status),
            payment_status: normalize(input.payment_status),
            route_search: search(input.route_search)?,
        };
        filter.base.employee_search = search(filter.base.employee_search)?;
        filter.validate(kind)?;
        Ok(filter)
    }

    pub fn validate(&self, kind: HrReportKind) -> KabiPayResult<()> {
        self.base.validate()?;
        let travel = kind == HrReportKind::TravelRequests;
        if travel && (self.expense_category_id.is_some() || self.payment_status.is_some()) {
            return Err(KabiPayError::Validation(
                "expense category and payment status do not apply to travel reports".into(),
            ));
        }
        if !travel && self.route_search.is_some() {
            return Err(KabiPayError::Validation(
                "route search only applies to travel reports".into(),
            ));
        }
        let statuses: &[&str] = if travel {
            &["PENDING", "APPROVED", "REJECTED"]
        } else {
            &["PENDING", "APPROVED", "PARTIAL_APPROVED", "REJECTED"]
        };
        validate_status(self.approval_status.as_deref(), statuses, "approvalStatus")?;
        if let Some(status) = self.payment_status.as_deref() {
            normalize_expense_payment_status_wire(status)
                .map_err(|_| KabiPayError::Validation("invalid paymentStatus".into()))?;
        }
        Ok(())
    }
}

fn parse_id(value: Option<async_graphql::ID>, name: &str) -> KabiPayResult<Option<Uuid>> {
    value
        .map(|id| {
            Uuid::parse_str(id.as_str())
                .map_err(|_| KabiPayError::Validation(format!("invalid {name}")))
        })
        .transpose()
}
fn normalize(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
}
fn search(value: Option<String>) -> KabiPayResult<Option<String>> {
    let value = value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if value.as_ref().is_some_and(|s| s.chars().count() > 200) {
        return Err(KabiPayError::Validation(
            "report search must be 200 characters or fewer".into(),
        ));
    }
    Ok(value)
}
fn validate_status(value: Option<&str>, allowed: &[&str], field: &str) -> KabiPayResult<()> {
    if value.is_some_and(|s| !allowed.contains(&s)) {
        return Err(KabiPayError::Validation(format!("invalid {field}")));
    }
    Ok(())
}

pub fn literal_substring(value: Option<&str>) -> Option<String> {
    value.map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}
