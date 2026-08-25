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
    PERM_ATTENDANCE_PUNCH_POLICY, PERM_EMPLOYEE_MANAGE, PERM_EMPLOYEE_WRITE, SCOPE_RES_ATTENDANCE,
    SCOPE_RES_EMPLOYEE, SCOPE_RES_EXPENSE, SCOPE_RES_LEAVE, SCOPE_RES_TIMESHEET,
};
pub use env_file::load_dotenv;
pub use error::{KabiPayError, KabiPayResult};
pub use pagination::{PageInfo, PageInput};
pub use subgraph::require_operator_context;
pub use tenant_seed::{
    deterministic_tenant_database_row_uuid, deterministic_tenant_uuid,
};
