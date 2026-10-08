use async_graphql::{Enum, InputObject, SimpleObject, ID};
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum HrReportKind {
    AttendancePunctuality,
    LeaveRequests,
    LeaveBalances,
    PayrollRegister,
    UnpaidLeave,
    EmployeeMovements,
    TimesheetHours,
    CompOffCredits,
    PendingRequests,
    ExpenseClaims,
    TravelRequests,
}

#[derive(InputObject, Default, Clone)]
pub struct ClaimTravelReportFilterInput {
    pub department_id: Option<ID>,
    pub location_id: Option<ID>,
    pub expense_category_id: Option<ID>,
    pub approval_status: Option<String>,
    pub payment_status: Option<String>,
    pub route_search: Option<String>,
}

#[derive(SimpleObject)]
pub struct ClaimTravelReportOption {
    pub id: ID,
    pub name: String,
}
#[derive(SimpleObject)]
pub struct ClaimTravelReportOptions {
    pub departments: Vec<ClaimTravelReportOption>,
    pub locations: Vec<ClaimTravelReportOption>,
    pub expense_categories: Vec<ClaimTravelReportOption>,
}
#[derive(SimpleObject)]
pub struct HrReportRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: i32,
}
#[derive(SimpleObject)]
pub struct HrReportCsv {
    pub file_name: String,
    pub csv: String,
    pub row_count: i32,
}
#[derive(SimpleObject, Default)]
pub struct HrInsights {
    pub on_time_days: Option<i32>,
    pub late_days: Option<i32>,
    pub unknown_punctuality_days: Option<i32>,
    pub incomplete_days: Option<i32>,
    pub joiners: Option<i32>,
    pub exits: Option<i32>,
    pub active_headcount: Option<i32>,
    pub net_salary_generated: Option<String>,
    pub generated_payslips: Option<i32>,
    pub pending_requests: Option<i32>,
    pub monthly_payroll: Option<Vec<HrMonthlyPayroll>>,
    pub included_pending_domains: Vec<String>,
}
#[derive(SimpleObject)]
pub struct HrMonthlyPayroll {
    pub month: String,
    pub net_salary_generated: String,
    pub payslips: i32,
}
