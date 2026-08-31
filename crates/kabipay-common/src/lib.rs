//! kabipay-common
//!
//! Shared primitives used by every KabiPay microservice:
//!   - Canonical error type ([`error::KabiPayError`])
//!   - Request contexts ([`context::OperatorContext`], [`context::ClientContext`])
//!   - Tenant database resolver ([`db::resolve_tenant_db`])
//!   - Axum middleware (auth for both planes, module-subscription guard)
//!   - Pagination helpers
//!   - Structured logging bootstrap
//!
//! Every service depends on this crate via `kabipay-common = { workspace = true }`.

pub mod client_data_scope;
pub mod context;
pub mod db_constraint;
pub mod due_offboarding;
pub mod db;
pub mod env_file;
pub mod error;
pub mod file_download_token;
pub mod ids;
pub mod jwt;
pub mod middleware;
pub mod pagination;
pub mod password;
pub mod private_file_cleanup;
pub mod subgraph;
pub mod telemetry;
pub mod tenant_business_clock;
pub mod tenant_seed;
pub mod workflow_approval;
pub mod workflow_current_step;
pub mod workflow_inbox;

pub use context::{
    ClientContext, ClientRequestHints, ClientViewerEmployee, OperatorContext, ScopeType,
    PERM_ATTENDANCE_PUNCH_POLICY, PERM_ATTENDANCE_PUNCH_SELF, PERM_ATTENDANCE_READ,
    PERM_ATTENDANCE_REGULARIZE, PERM_EMPLOYEE_DIRECTORY_READ, PERM_EMPLOYEE_MANAGE,
    PERM_EMPLOYEE_READ, PERM_EMPLOYEE_WRITE, PERM_EXPENSE_APPROVE, PERM_EXPENSE_MANAGE,
    PERM_EXPENSE_PAY, PERM_EXPENSE_READ, PERM_EXPENSE_SUBMIT, PERM_LEAVE_APPROVE,
    PERM_LEAVE_MANAGE, PERM_LEAVE_READ, PERM_LEAVE_SUBMIT, PERM_NOTIFICATION_MANAGE,
    PERM_NOTIFICATION_READ, PERM_PAYROLL_MANAGE, PERM_PAYROLL_READ,
    PERM_PAYROLL_STATUTORY_EXPORT, PERM_ROLE_MANAGE, PERM_TAX_APPROVE, PERM_TAX_MANAGE,
    PERM_TAX_READ, PERM_TAX_SUBMIT, PERM_TIMESHEET_APPROVE, PERM_TIMESHEET_MANAGE,
    PERM_TIMESHEET_READ, PERM_TIMESHEET_WRITE, PERM_TRAVEL_APPROVE, PERM_TRAVEL_MANAGE,
    PERM_TRAVEL_READ, PERM_TRAVEL_SUBMIT, PERM_WORKFLOW_MANAGE,
    SCOPE_RES_ATTENDANCE, SCOPE_RES_EMPLOYEE, SCOPE_RES_EXPENSE, SCOPE_RES_LEAVE,
    SCOPE_RES_TIMESHEET,
};
pub use env_file::load_dotenv;
pub use error::{KabiPayError, KabiPayResult};
pub use pagination::{PageInfo, PageInput};
pub use subgraph::require_operator_context;
pub use tenant_seed::{
    deterministic_tenant_database_row_uuid, deterministic_tenant_uuid,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_permission_vocabulary_is_exported_from_the_common_crate() {
        let exported = [
            PERM_EMPLOYEE_DIRECTORY_READ,
            PERM_EMPLOYEE_READ,
            PERM_EMPLOYEE_WRITE,
            PERM_EMPLOYEE_MANAGE,
            PERM_ATTENDANCE_READ,
            PERM_ATTENDANCE_PUNCH_SELF,
            PERM_ATTENDANCE_REGULARIZE,
            PERM_ATTENDANCE_PUNCH_POLICY,
            PERM_TIMESHEET_READ,
            PERM_TIMESHEET_WRITE,
            PERM_TIMESHEET_APPROVE,
            PERM_TIMESHEET_MANAGE,
            PERM_LEAVE_READ,
            PERM_LEAVE_SUBMIT,
            PERM_LEAVE_APPROVE,
            PERM_LEAVE_MANAGE,
            PERM_EXPENSE_READ,
            PERM_EXPENSE_SUBMIT,
            PERM_EXPENSE_APPROVE,
            PERM_EXPENSE_MANAGE,
            PERM_EXPENSE_PAY,
            PERM_TRAVEL_READ,
            PERM_TRAVEL_SUBMIT,
            PERM_TRAVEL_APPROVE,
            PERM_TRAVEL_MANAGE,
            PERM_PAYROLL_READ,
            PERM_PAYROLL_MANAGE,
            PERM_PAYROLL_STATUTORY_EXPORT,
            PERM_TAX_READ,
            PERM_TAX_SUBMIT,
            PERM_TAX_APPROVE,
            PERM_TAX_MANAGE,
            PERM_NOTIFICATION_READ,
            PERM_NOTIFICATION_MANAGE,
            PERM_ROLE_MANAGE,
            PERM_WORKFLOW_MANAGE,
        ];

        assert_eq!(exported.len(), 36);
    }
}
