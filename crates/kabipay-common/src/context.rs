//! Request contexts injected by auth middleware.
//!
//! Two planes, two contexts. JWTs issued by the two planes MUST NOT be interchangeable
//! (different `iss` claim, different signing secret, validated by different middleware).

use crate::{KabiPayError, KabiPayResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Canonical employee status that permits login and active employment workflows.
pub const EMPLOYMENT_STATUS_ACTIVE: &str = "ACTIVE";
/// Canonical probation status; probationary employees remain actively employed.
pub const EMPLOYMENT_STATUS_PROBATION: &str = "PROBATION";
pub const EMPLOYMENT_STATUS_INACTIVE: &str = "INACTIVE";
pub const EMPLOYMENT_STATUS_ON_LEAVE: &str = "ON_LEAVE";
pub const EMPLOYMENT_STATUS_SUSPENDED: &str = "SUSPENDED";
pub const EMPLOYMENT_STATUS_TERMINATED: &str = "TERMINATED";
/// Complete set of employment statuses accepted at service write boundaries.
pub const EMPLOYMENT_STATUSES: [&str; 6] = [
    EMPLOYMENT_STATUS_ACTIVE,
    EMPLOYMENT_STATUS_PROBATION,
    EMPLOYMENT_STATUS_INACTIVE,
    EMPLOYMENT_STATUS_ON_LEAVE,
    EMPLOYMENT_STATUS_SUSPENDED,
    EMPLOYMENT_STATUS_TERMINATED,
];
/// Canonical statuses treated as active employment across authentication and HRMS domains.
pub const ACTIVE_EMPLOYMENT_STATUSES: [&str; 2] = [
    EMPLOYMENT_STATUS_ACTIVE,
    EMPLOYMENT_STATUS_PROBATION,
];

/// Normalize a supported employment status to its canonical persisted representation.
pub fn canonical_employment_status(status: &str) -> KabiPayResult<&'static str> {
    match status.trim().to_ascii_uppercase().as_str() {
        EMPLOYMENT_STATUS_ACTIVE => Ok(EMPLOYMENT_STATUS_ACTIVE),
        EMPLOYMENT_STATUS_PROBATION => Ok(EMPLOYMENT_STATUS_PROBATION),
        EMPLOYMENT_STATUS_INACTIVE => Ok(EMPLOYMENT_STATUS_INACTIVE),
        EMPLOYMENT_STATUS_ON_LEAVE => Ok(EMPLOYMENT_STATUS_ON_LEAVE),
        EMPLOYMENT_STATUS_SUSPENDED => Ok(EMPLOYMENT_STATUS_SUSPENDED),
        EMPLOYMENT_STATUS_TERMINATED => Ok(EMPLOYMENT_STATUS_TERMINATED),
        _ => Err(KabiPayError::Validation(
            "employment status must be ACTIVE, PROBATION, INACTIVE, ON_LEAVE, SUSPENDED, or TERMINATED"
                .into(),
        )),
    }
}

/// Returns whether an employee status represents current active employment.
pub fn is_active_employment_status(status: &str) -> bool {
    canonical_employment_status(status)
        .is_ok_and(|status| ACTIVE_EMPLOYMENT_STATUSES.contains(&status))
}

#[cfg(test)]
mod active_employment_tests {
    use super::*;

    #[test]
    fn employment_status_conversion_normalizes_every_supported_status() {
        for (input, expected) in [
            (" active ", EMPLOYMENT_STATUS_ACTIVE),
            ("Probation", EMPLOYMENT_STATUS_PROBATION),
            ("inactive", EMPLOYMENT_STATUS_INACTIVE),
            (" on_leave ", EMPLOYMENT_STATUS_ON_LEAVE),
            ("Suspended", EMPLOYMENT_STATUS_SUSPENDED),
            ("terminated", EMPLOYMENT_STATUS_TERMINATED),
        ] {
            assert_eq!(
                canonical_employment_status(input).expect("supported employment status"),
                expected,
                "input={input}"
            );
        }
        assert_eq!(
            EMPLOYMENT_STATUSES,
            [
                "ACTIVE",
                "PROBATION",
                "INACTIVE",
                "ON_LEAVE",
                "SUSPENDED",
                "TERMINATED",
            ]
        );
    }

    #[test]
    fn employment_status_conversion_rejects_empty_and_unknown_values() {
        for input in ["", "   ", "NOTICE", "ACTIVE_EMPLOYEE"] {
            let error = canonical_employment_status(input)
                .expect_err("unsupported employment status must be rejected");
            assert_eq!(error.code(), "VALIDATION_ERROR", "input={input}");
        }
    }

    #[test]
    fn canonical_active_employment_accepts_only_active_and_probation() {
        for status in ["ACTIVE", "active", " PROBATION ", "probation"] {
            assert!(is_active_employment_status(status), "status={status}");
        }
        for status in ["INACTIVE", "TERMINATED", "NOTICE", "", "ACTIVE_EMPLOYEE"] {
            assert!(!is_active_employment_status(status), "status={status}");
        }
        assert_eq!(ACTIVE_EMPLOYMENT_STATUSES, ["ACTIVE", "PROBATION"]);
    }
}

/// Data-level access control scope. Applied per exact permission per role via `PERMISSION_SCOPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ScopeType {
    /// User can only see/edit their own records.
    Self_,
    /// Manager can see their direct reports (resolved via EMPLOYEE_HIERARCHY).
    Team,
    /// HR user can see everyone in their department.
    Department,
    /// Unrestricted within tenant (HR admin, payroll admin).
    All,
}

impl ScopeType {
    /// Wider access wins when merging several role rows for the same exact permission.
    pub fn rank(self) -> u8 {
        match self {
            ScopeType::Self_ => 1,
            ScopeType::Team => 2,
            ScopeType::Department => 3,
            ScopeType::All => 4,
        }
    }

    /// Parse a DB or JWT `scope_type` string (case-insensitive).
    pub fn parse_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "SELF" => Some(ScopeType::Self_),
            "TEAM" => Some(ScopeType::Team),
            "DEPARTMENT" => Some(ScopeType::Department),
            "ALL" => Some(ScopeType::All),
            _ => None,
        }
    }

    pub fn to_wire(self) -> &'static str {
        match self {
            ScopeType::Self_ => "SELF",
            ScopeType::Team => "TEAM",
            ScopeType::Department => "DEPARTMENT",
            ScopeType::All => "ALL",
        }
    }
}

/// `permission` table `resource` values used for `permission_scope` + list filtering.
pub const SCOPE_RES_EMPLOYEE: &str = "employee";
/// Leave requests and balances roll up under the leave module resource.
pub const SCOPE_RES_LEAVE: &str = "leave";
/// Expense claims — list/filter scope (M10); align `permission_scope.resource`.
pub const SCOPE_RES_EXPENSE: &str = "expense";
/// Attendance punches, regularization lists, **`timesheet_entry`** rows — `permission_scope.resource`.
pub const SCOPE_RES_ATTENDANCE: &str = "attendance";
/// **`timesheet_week_batches`** approval queue — must match `permission_scope` seeds (`timesheet` + `approve`), not `attendance`.
pub const SCOPE_RES_TIMESHEET: &str = "timesheet";

/// The caller’s employee row fields needed for `TEAM` / `DEPARTMENT` list filters.
#[derive(Debug, Clone, Copy)]
pub struct ClientViewerEmployee {
    pub employee_id: Uuid,
    pub department_id: Option<Uuid>,
}

/// Context attached to every operator-plane request after `operator_auth` middleware runs.
/// Isolated from `ClientContext` — the two must never be interchangeable.
#[derive(Debug, Clone)]
pub struct OperatorContext {
    pub operator_user_id: Uuid,
    pub roles: Vec<String>,
    /// Tenants this operator has scoped access to. Empty vector = super admin (all tenants).
    pub tenant_access: Vec<Uuid>,
}

impl OperatorContext {
    pub fn is_super_admin(&self) -> bool {
        self.tenant_access.is_empty()
    }

    pub fn can_access_tenant(&self, tenant_id: Uuid) -> bool {
        self.is_super_admin() || self.tenant_access.contains(&tenant_id)
    }
}

/// Context attached to every client-plane request after `client_auth` middleware runs.
///
/// ALWAYS contains `tenant_id`. Every SeaORM query in a client service MUST filter by
/// this tenant_id — even though schema isolation already protects, it's defense in depth.
#[derive(Debug, Clone)]
pub struct ClientContext {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    /// Resolved EMPLOYEE.id if the user is linked to an employee record.
    pub employee_id: Option<Uuid>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    /// Per-resource scope map: resource => ScopeType.
    /// Resolvers apply this to filter queries before returning data.
    pub scopes: std::collections::HashMap<String, ScopeType>,
}

impl ClientContext {
    /// Returns `true` if the user has any of the provided permissions (OR semantics).
    pub fn has_any_permission(&self, perms: &[&str]) -> bool {
        perms
            .iter()
            .any(|p| self.permissions.iter().any(|owned| owned == p))
    }

    /// Returns the effective scope for a resource, defaulting to `Self_` if no scope is defined.
    pub fn scope_for(&self, resource: &str) -> ScopeType {
        self.scopes
            .get(resource)
            .copied()
            .unwrap_or(ScopeType::Self_)
    }
}

/// JWT claims for an operator token.
///
/// `roles` / `tenant_access` default to empty so tokens issued by an early
/// version of `kabipay-auth` (before RBAC is fully wired) still round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorClaims {
    pub sub: Uuid,
    pub iss: String,
    pub exp: i64,
    pub iat: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub tenant_access: Vec<Uuid>,
}

/// JWT claims for a client token.
///
/// `employee_id` / `roles` / `permissions` default to empty / None so
/// tokens issued by an early version of `kabipay-auth` (before RBAC is
/// fully wired) still round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientClaims {
    pub sub: Uuid,
    pub iss: String,
    pub exp: i64,
    pub iat: i64,
    pub tenant_id: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<Uuid>,
    #[serde(default)]
    pub must_change_password: bool,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Widest scope for each exact permission code, such as `attendance:read`.
    #[serde(default)]
    pub permission_scopes: HashMap<String, String>,
    /// Legacy per-resource scope field retained for token compatibility. New tokens do not derive
    /// authorization from it; scoped decisions use `permission_scopes`.
    #[serde(default)]
    pub resource_scopes: HashMap<String, String>,
}

pub const OPERATOR_JWT_ISSUER: &str = "kabipay-ops";
pub const CLIENT_JWT_ISSUER: &str = "kabipay-client";

/// Tenant-wide non-sensitive employee directory visibility.
pub const PERM_EMPLOYEE_DIRECTORY_READ: &str = "employee_directory:read";
/// JWT `permissions` claim uses `resource:action` to match `permission` rows.
pub const PERM_EMPLOYEE_WRITE: &str = "employee:write";
pub const PERM_EMPLOYEE_READ: &str = "employee:read";
/// Broader org directory edits (e.g. bulk / sensitive fields) — same gate as write for now.
pub const PERM_EMPLOYEE_MANAGE: &str = "employee:manage";
/// Approve or reject other users' leave requests.
pub const PERM_LEAVE_APPROVE: &str = "leave:approve";
pub const PERM_LEAVE_READ: &str = "leave:read";
pub const PERM_LEAVE_SUBMIT: &str = "leave:submit";
/// Approve or reject expense claims submitted by others.
pub const PERM_EXPENSE_APPROVE: &str = "expense:approve";
pub const PERM_EXPENSE_READ: &str = "expense:read";
pub const PERM_EXPENSE_SUBMIT: &str = "expense:submit";
/// Configure expense categories (travel/meal/other claim types employees select).
pub const PERM_EXPENSE_MANAGE: &str = "expense:manage";
/// Mark expense reimbursements as paid / failed / on hold (payroll or accounting path).
pub const PERM_EXPENSE_PAY: &str = "expense:pay";
/// Approve or reject **tax proof** lines (submitted actuals vs declared deductions).
pub const PERM_TAX_APPROVE: &str = "tax:approve";
/// Backward-compatible name for the canonical `tax:approve` permission.
pub const PERM_TAX_PROOF_APPROVE: &str = PERM_TAX_APPROVE;
pub const PERM_TAX_READ: &str = "tax:read";
pub const PERM_TAX_SUBMIT: &str = "tax:submit";
pub const PERM_TAX_MANAGE: &str = "tax:manage";
/// Export India payroll statutory artefacts (e.g. monthly TDS summary CSV) for the tenant.
pub const PERM_PAYROLL_STATUTORY_EXPORT: &str = "payroll:statutory_export";
pub const PERM_PAYROLL_READ: &str = "payroll:read";
pub const PERM_PAYROLL_MANAGE: &str = "payroll:manage";
/// Configure live punch enforcement (geofence / IP allowlist) for the tenant.
pub const PERM_ATTENDANCE_PUNCH_POLICY: &str = "attendance:punch_policy";
pub const PERM_ATTENDANCE_READ: &str = "attendance:read";
/// Create or edit **workflow** definitions and **steps** (tenant configuration).
pub const PERM_WORKFLOW_MANAGE: &str = "workflow:manage";
/// Configure **leave** master data (types, policies, balances) and holiday calendars (attendance subgraph).
pub const PERM_LEAVE_MANAGE: &str = "leave:manage";
/// Assign tenant **roles** / **permissions** / **scopes** to users (RBAC administration).
pub const PERM_ROLE_MANAGE: &str = "role:manage";
/// Workplace: configure benefit types/plans and tenant-wide enrollment views.
pub const PERM_BENEFITS_MANAGE: &str = "benefits:manage";
/// Self-service: view active offerings and enroll in benefit plans.
pub const PERM_BENEFITS_SELF: &str = "benefits:self";
/// Workplace: job postings and candidate applications (talent acquisition console).
pub const PERM_RECRUITMENT_MANAGE: &str = "recruitment:manage";
/// Workplace: onboarding/offboarding HR console (tenant-wide separations, approvals depth).
pub const PERM_ONBOARDING_MANAGE: &str = "onboarding:manage";
/// Workplace: employee self-service for join tasks and filing own separation (route + list scope).
pub const PERM_ONBOARDING_SELF: &str = "onboarding:self";
/// Workplace: performance cycles and goals administration.
pub const PERM_PERFORMANCE_MANAGE: &str = "performance:manage";
/// Workplace: LMS skills and courses administration.
pub const PERM_LEARNING_MANAGE: &str = "learning:manage";
/// Workplace: asset categories and assignments registry.
pub const PERM_ASSETS_MANAGE: &str = "assets:manage";
/// Workplace: read tenant-wide asset inventory and employee assignments.
pub const PERM_ASSETS_READ: &str = "assets:read";
/// Self-service: view assets assigned to the signed-in employee.
pub const PERM_ASSETS_SELF: &str = "assets:self";
/// Workplace: view/manage tenant-wide grievance cases (beyond own submissions).
pub const PERM_GRIEVANCE_MANAGE: &str = "grievance:manage";
/// Self-service: file grievances and view own cases/categories.
pub const PERM_GRIEVANCE_SELF: &str = "grievance:self";
/// Workplace: succession competencies and talent pools.
pub const PERM_SUCCESSION_MANAGE: &str = "succession:manage";
/// Workplace: salary bands and compensation review cycles (distinct from payslip payroll).
pub const PERM_COMPENSATION_MANAGE: &str = "compensation:manage";
/// View Insights / workforce analytics (`report_definitions`, dashboards, snapshots).
pub const PERM_ANALYTICS_READ: &str = "analytics:read";
/// Record live punches and read **own** punch-day summary (`punch_today`, `punchDaySummary`).
pub const PERM_ATTENDANCE_PUNCH_SELF: &str = "attendance:punch_self";
/// Correct missed punches beyond the self-service window (manager / HR path).
pub const PERM_ATTENDANCE_REGULARIZE: &str = "attendance:regularize";
/// Approve or reject submitted weekly timesheets.
pub const PERM_TIMESHEET_APPROVE: &str = "timesheet:approve";
pub const PERM_TIMESHEET_READ: &str = "timesheet:read";
pub const PERM_TIMESHEET_WRITE: &str = "timesheet:write";
/// Configure timesheet catalogs (projects / tasks) and lock policy (`master_data` backed).
pub const PERM_TIMESHEET_MANAGE: &str = "timesheet:manage";
/// Create or edit **tenant announcements**, send **direct in-app notifications**, and remove broadcasts.
pub const PERM_NOTIFICATION_MANAGE: &str = "notification:manage";
pub const PERM_NOTIFICATION_READ: &str = "notification:read";
pub const PERM_TRAVEL_READ: &str = "travel:read";
pub const PERM_TRAVEL_SUBMIT: &str = "travel:submit";
pub const PERM_TRAVEL_APPROVE: &str = "travel:approve";
pub const PERM_TRAVEL_MANAGE: &str = "travel:manage";

/// HTTP-derived metadata attached to each GraphQL request by [`crate::subgraph::tenant_graphql_post`].
/// Values come from gateway headers, not from GraphQL variables (so they are suitable for policy).
#[derive(Clone, Debug, Default)]
pub struct ClientRequestHints {
    /// First hop from `X-Forwarded-For`, else `X-Real-IP`, when present.
    pub client_ip: Option<String>,
    /// Gateway correlation identifier, normalized at the HTTP boundary.
    pub request_id: Option<String>,
}

impl ClientClaims {
    /// True if the token includes one of the permission strings (exact match on wire).
    pub fn has_any_permission(&self, perms: &[&str]) -> bool {
        perms
            .iter()
            .any(|p| self.permissions.iter().any(|owned| owned == p))
    }

    fn has_permission_with_scope(&self, permission: &str, scope: ScopeType) -> bool {
        self.has_any_permission(&[permission])
            && self.scope_for_permission(permission) == Some(scope)
    }

    /// Create/update other users' **employee** rows (not self-service profile edits).
    pub fn can_manage_employee_directory(&self) -> bool {
        self.has_any_permission(&[PERM_EMPLOYEE_WRITE, PERM_EMPLOYEE_MANAGE])
    }

    /// Approve or reject **leave** requests (not the employee's own self-service only path).
    pub fn can_approve_leave(&self) -> bool {
        self.has_any_permission(&[PERM_LEAVE_APPROVE])
    }

    /// Approve or reject **expense** claims (approver/manager path).
    pub fn can_approve_expense(&self) -> bool {
        self.has_any_permission(&[PERM_EXPENSE_APPROVE])
    }

    /// Update expense **payment** / reimbursement status after approval.
    pub fn can_mark_expense_payment(&self) -> bool {
        self.has_any_permission(&[PERM_EXPENSE_PAY])
    }

    /// Approve or reject **tax deduction proof** lines (documented actuals).
    pub fn can_approve_tax_proof(&self) -> bool {
        self.has_any_permission(&[PERM_TAX_PROOF_APPROVE])
    }

    /// Download tenant-wide **statutory payroll** reports (TDS summary CSV, etc.).
    pub fn can_export_payroll_statutory(&self) -> bool {
        self.has_any_permission(&[PERM_PAYROLL_STATUTORY_EXPORT])
    }

    /// Configure **live punch** policy (geofence + IP allowlist) for the tenant.
    pub fn can_configure_attendance_punch_policy(&self) -> bool {
        self.has_any_permission(&[PERM_ATTENDANCE_PUNCH_POLICY])
    }

    /// Create or update **workflow** definitions and **steps** (tenant approval graphs).
    pub fn can_manage_workflow_definitions(&self) -> bool {
        self.has_any_permission(&[PERM_WORKFLOW_MANAGE])
    }

    /// Configure leave types, policies, employee balances, and (via attendance) holiday calendars.
    pub fn can_manage_leave_configuration(&self) -> bool {
        self.has_any_permission(&[PERM_LEAVE_MANAGE])
    }

    /// Configure expense claim categories master data (`expense_category`).
    pub fn can_manage_expense_configuration(&self) -> bool {
        self.has_any_permission(&[PERM_EXPENSE_MANAGE])
    }

    /// Manage tenant RBAC: roles, permission grants, and list scopes (`role:manage`).
    pub fn can_manage_tenant_rbac(&self) -> bool {
        self.has_any_permission(&[PERM_ROLE_MANAGE])
    }

    pub fn can_manage_benefits_catalog(&self) -> bool {
        self.has_permission_with_scope(PERM_BENEFITS_MANAGE, ScopeType::All)
    }

    /// Benefit types/plans list queries for the workplace Benefits UI (HR + enrollment pickers).
    pub fn can_read_benefit_catalog_queries(&self) -> bool {
        self.can_manage_benefits_catalog()
            || self.has_permission_with_scope(PERM_BENEFITS_SELF, ScopeType::Self_)
    }

    /// Read or change only the signed-in employee's own benefit enrollments.
    pub fn can_use_benefits_self_service(&self) -> bool {
        self.can_read_benefit_catalog_queries()
    }

    pub fn can_manage_recruitment(&self) -> bool {
        self.has_permission_with_scope(PERM_RECRUITMENT_MANAGE, ScopeType::All)
    }

    pub fn can_manage_performance_programs(&self) -> bool {
        self.has_permission_with_scope(PERM_PERFORMANCE_MANAGE, ScopeType::All)
    }

    pub fn can_manage_learning_catalog(&self) -> bool {
        self.has_permission_with_scope(PERM_LEARNING_MANAGE, ScopeType::All)
    }

    pub fn can_manage_assets_registry(&self) -> bool {
        self.has_any_permission(&[PERM_ASSETS_MANAGE])
    }

    pub fn can_read_assets_registry(&self) -> bool {
        self.can_manage_assets_registry() || self.has_any_permission(&[PERM_ASSETS_READ])
    }

    pub fn can_read_own_assets(&self) -> bool {
        self.can_read_assets_registry() || self.has_any_permission(&[PERM_ASSETS_SELF])
    }

    pub fn can_manage_succession_planning(&self) -> bool {
        self.has_permission_with_scope(PERM_SUCCESSION_MANAGE, ScopeType::All)
    }

    pub fn can_manage_compensation_admin(&self) -> bool {
        self.has_permission_with_scope(PERM_COMPENSATION_MANAGE, ScopeType::All)
    }

    pub fn can_manage_grievance_tenant_cases(&self) -> bool {
        self.has_any_permission(&[PERM_GRIEVANCE_MANAGE])
    }

    /// Submit grievances and view **own** cases (`grievance:self` or `grievance:manage`).
    pub fn can_use_grievance_self_service(&self) -> bool {
        self.has_any_permission(&[PERM_GRIEVANCE_SELF, PERM_GRIEVANCE_MANAGE])
    }

    /// Tenant-wide onboarding/offboarding lists and HR depth (`onboarding:manage`).
    pub fn can_manage_onboarding_tenant(&self) -> bool {
        self.has_any_permission(&[PERM_ONBOARDING_MANAGE])
    }

    /// Join checklist + own separation flows (`onboarding:self` or `onboarding:manage`).
    pub fn can_use_onboarding_self_service(&self) -> bool {
        self.has_any_permission(&[PERM_ONBOARDING_SELF, PERM_ONBOARDING_MANAGE])
    }

    /// Workforce insights UI (`analytics:read`) — dashboards, report catalog, snapshots.
    pub fn can_access_analytics_insights(&self) -> bool {
        self.has_any_permission(&[PERM_ANALYTICS_READ])
    }

    /// Own-device punches require the explicit `attendance:punch_self` permission.
    pub fn can_record_own_attendance_punches(&self) -> bool {
        self.has_any_permission(&[PERM_ATTENDANCE_PUNCH_SELF])
    }

    /// Manual attendance corrections beyond the configured employee window (`attendance:regularize`).
    pub fn can_regularize_attendance_records(&self) -> bool {
        self.has_any_permission(&[PERM_ATTENDANCE_REGULARIZE])
    }

    /// Approve weekly timesheet batches (`timesheet:approve`), analogous to leave approval.
    pub fn can_approve_timesheet_requests(&self) -> bool {
        self.has_any_permission(&[PERM_TIMESHEET_APPROVE])
    }

    /// HR configuration for timesheet projects/tasks and lock JSON (`timesheet:manage`).
    pub fn can_manage_timesheet_configuration(&self) -> bool {
        self.has_any_permission(&[PERM_TIMESHEET_MANAGE])
    }

    /// HR / comms admin: announcements, direct notifications, and related deletes. Also accepts
    /// split permissions from the RBAC catalog (`notification:*`).
    pub fn can_manage_notifications(&self) -> bool {
        self.has_any_permission(&[
            PERM_NOTIFICATION_MANAGE,
            "notification:create",
            "notification:update",
            "notification:delete",
        ])
    }

    /// Effective data scope for list/detail filters (`permission_scope` merged at login). Defaults
    /// to `Self_` when unset (legacy tokens and least-privilege default).
    pub fn data_scope(&self, resource: &str) -> ScopeType {
        self.explicit_data_scope(resource).unwrap_or(ScopeType::Self_)
    }

    /// Parsed JWT scope only when this resource has an explicit valid value.
    /// Managed operations use this to fail closed without changing the legacy
    /// `data_scope` SELF default used by self-service flows.
    pub fn explicit_data_scope(&self, resource: &str) -> Option<ScopeType> {
        self.resource_scopes
            .get(resource)
            .and_then(|scope| ScopeType::parse_loose(scope))
    }

    /// Parsed scope for one exact permission. Missing or malformed values remain absent so
    /// callers cannot silently broaden or default scoped authorization.
    pub fn scope_for_permission(&self, permission: &str) -> Option<ScopeType> {
        let key = permission.trim().to_ascii_lowercase();
        self.permission_scopes
            .get(&key)
            .and_then(|scope| ScopeType::parse_loose(scope))
    }

    /// Compatibility alias for callers that already use the explicit-scope name.
    pub fn explicit_scope_for_permission(&self, permission: &str) -> Option<ScopeType> {
        self.scope_for_permission(permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_claims(roles: &[&str], permissions: &[&str]) -> ClientClaims {
        ClientClaims {
            sub: Uuid::nil(),
            iss: CLIENT_JWT_ISSUER.to_string(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::nil(),
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: roles.iter().map(|role| (*role).to_string()).collect(),
            permissions: permissions
                .iter()
                .map(|permission| (*permission).to_string())
                .collect(),
            permission_scopes: HashMap::new(),
            resource_scopes: HashMap::new(),
        }
    }

    #[test]
    fn canonical_permission_constants_match_the_runtime_vocabulary() {
        let actual = [
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
        let expected = [
            "employee_directory:read",
            "employee:read",
            "employee:write",
            "employee:manage",
            "attendance:read",
            "attendance:punch_self",
            "attendance:regularize",
            "attendance:punch_policy",
            "timesheet:read",
            "timesheet:write",
            "timesheet:approve",
            "timesheet:manage",
            "leave:read",
            "leave:submit",
            "leave:approve",
            "leave:manage",
            "expense:read",
            "expense:submit",
            "expense:approve",
            "expense:manage",
            "expense:pay",
            "travel:read",
            "travel:submit",
            "travel:approve",
            "travel:manage",
            "payroll:read",
            "payroll:manage",
            "payroll:statutory_export",
            "tax:read",
            "tax:submit",
            "tax:approve",
            "tax:manage",
            "notification:read",
            "notification:manage",
            "role:manage",
            "workflow:manage",
        ];

        assert_eq!(actual, expected);
    }

    #[test]
    fn employee_read_scope_does_not_accept_retired_employee_self() {
        let mut current = client_claims(&[], &[PERM_EMPLOYEE_READ]);
        current
            .permission_scopes
            .insert(PERM_EMPLOYEE_READ.into(), "SELF".into());

        assert!(current.has_any_permission(&[PERM_EMPLOYEE_READ]));
        assert_eq!(
            current.scope_for_permission(PERM_EMPLOYEE_READ),
            Some(ScopeType::Self_)
        );

        let mut retired = client_claims(&[], &["employee:self"]);
        retired
            .permission_scopes
            .insert("employee:self".into(), "SELF".into());

        assert!(!retired.has_any_permission(&[PERM_EMPLOYEE_READ]));
        assert_eq!(retired.scope_for_permission(PERM_EMPLOYEE_READ), None);
    }

    #[test]
    fn exact_permission_scope_is_missing_when_absent_or_malformed() {
        let mut claims = client_claims(&[], &[PERM_LEAVE_READ]);

        assert_eq!(claims.scope_for_permission(PERM_LEAVE_READ), None);

        claims
            .permission_scopes
            .insert(PERM_LEAVE_READ.into(), "INVALID".into());
        assert_eq!(claims.scope_for_permission(PERM_LEAVE_READ), None);
    }

    #[test]
    fn exact_permission_scope_does_not_inherit_from_other_actions_or_resources() {
        let mut claims = client_claims(&[], &[PERM_LEAVE_APPROVE]);
        claims
            .permission_scopes
            .insert(PERM_LEAVE_APPROVE.into(), "TEAM".into());
        claims
            .resource_scopes
            .insert(SCOPE_RES_LEAVE.into(), "ALL".into());
        claims
            .resource_scopes
            .insert(SCOPE_RES_EXPENSE.into(), "ALL".into());

        assert_eq!(
            claims.scope_for_permission(PERM_LEAVE_APPROVE),
            Some(ScopeType::Team)
        );
        assert_eq!(claims.scope_for_permission(PERM_LEAVE_READ), None);
        assert_eq!(claims.scope_for_permission(PERM_EXPENSE_READ), None);
    }

    #[test]
    fn self_punch_permission_grants_own_attendance_punch_access() {
        let claims = client_claims(&[], &[PERM_ATTENDANCE_PUNCH_SELF]);

        assert!(claims.can_record_own_attendance_punches());
    }

    #[test]
    fn workplace_configuration_capabilities_require_the_exact_all_scoped_permission() {
        let capabilities: [(&str, fn(&ClientClaims) -> bool); 6] = [
            (PERM_BENEFITS_MANAGE, ClientClaims::can_manage_benefits_catalog),
            (PERM_RECRUITMENT_MANAGE, ClientClaims::can_manage_recruitment),
            (
                PERM_PERFORMANCE_MANAGE,
                ClientClaims::can_manage_performance_programs,
            ),
            (PERM_LEARNING_MANAGE, ClientClaims::can_manage_learning_catalog),
            (
                PERM_SUCCESSION_MANAGE,
                ClientClaims::can_manage_succession_planning,
            ),
            (
                PERM_COMPENSATION_MANAGE,
                ClientClaims::can_manage_compensation_admin,
            ),
        ];

        for (permission, capability) in capabilities {
            assert!(!capability(&client_claims(&[], &[])), "{permission}");

            let mut missing_scope = client_claims(&[], &[permission]);
            assert!(!capability(&missing_scope), "{permission} without scope");

            for scope in ["SELF", "TEAM", "DEPARTMENT", "invalid"] {
                missing_scope
                    .permission_scopes
                    .insert(permission.into(), scope.into());
                assert!(!capability(&missing_scope), "{permission} with {scope}");
            }

            missing_scope
                .permission_scopes
                .insert(permission.into(), "ALL".into());
            assert!(capability(&missing_scope), "{permission} with ALL");
        }
    }

    #[test]
    fn benefits_self_service_accepts_only_self_scope_or_all_scoped_management() {
        let mut self_service = client_claims(&[], &[PERM_BENEFITS_SELF]);
        self_service
            .permission_scopes
            .insert(PERM_BENEFITS_SELF.into(), "SELF".into());
        assert!(self_service.can_read_benefit_catalog_queries());
        assert!(self_service.can_use_benefits_self_service());

        for scope in ["TEAM", "DEPARTMENT", "ALL", "invalid"] {
            self_service
                .permission_scopes
                .insert(PERM_BENEFITS_SELF.into(), scope.into());
            assert!(!self_service.can_read_benefit_catalog_queries(), "{scope}");
            assert!(!self_service.can_use_benefits_self_service(), "{scope}");
        }

        let mut manager = client_claims(&[], &[PERM_BENEFITS_MANAGE]);
        manager
            .permission_scopes
            .insert(PERM_BENEFITS_MANAGE.into(), "ALL".into());
        assert!(manager.can_read_benefit_catalog_queries());
        assert!(manager.can_use_benefits_self_service());
    }

    #[test]
    fn administrator_role_does_not_replace_self_punch_permission() {
        let claims = client_claims(&["HR_ADMIN"], &[]);

        assert!(!claims.can_record_own_attendance_punches());
    }

    #[test]
    fn administrator_role_does_not_grant_runtime_capabilities_without_permissions() {
        let claims = client_claims(&["TENANT_ADMIN", "HR_ADMIN", "ORG_ADMIN"], &[]);

        assert!(!claims.can_manage_employee_directory());
        assert!(!claims.can_approve_leave());
        assert!(!claims.can_approve_expense());
        assert!(!claims.can_mark_expense_payment());
        assert!(!claims.can_approve_tax_proof());
        assert!(!claims.can_export_payroll_statutory());
        assert!(!claims.can_configure_attendance_punch_policy());
        assert!(!claims.can_manage_workflow_definitions());
        assert!(!claims.can_manage_leave_configuration());
        assert!(!claims.can_manage_expense_configuration());
        assert!(!claims.can_manage_tenant_rbac());
        assert!(!claims.can_manage_benefits_catalog());
        assert!(!claims.can_manage_recruitment());
        assert!(!claims.can_manage_performance_programs());
        assert!(!claims.can_manage_learning_catalog());
        assert!(!claims.can_manage_assets_registry());
        assert!(!claims.can_manage_succession_planning());
        assert!(!claims.can_manage_compensation_admin());
        assert!(!claims.can_manage_grievance_tenant_cases());
        assert!(!claims.can_manage_onboarding_tenant());
        assert!(!claims.can_access_analytics_insights());
        assert!(!claims.can_approve_timesheet_requests());
        assert!(!claims.can_manage_timesheet_configuration());
        assert!(!claims.can_manage_notifications());
    }

    #[test]
    fn employee_directory_permission_does_not_replace_self_punch_permission() {
        let claims = client_claims(&[], &[PERM_EMPLOYEE_WRITE]);

        assert!(!claims.can_record_own_attendance_punches());
    }

    #[test]
    fn attendance_regularize_requires_explicit_permission() {
        assert!(client_claims(&[], &[PERM_ATTENDANCE_REGULARIZE])
            .can_regularize_attendance_records());
        assert!(!client_claims(&["HR_ADMIN"], &[])
            .can_regularize_attendance_records());
        assert!(!client_claims(&[], &[PERM_EMPLOYEE_MANAGE])
            .can_regularize_attendance_records());
    }

    #[test]
    fn explicit_data_scope_requires_a_present_valid_scope() {
        let mut claims = client_claims(&[], &[]);
        assert_eq!(claims.explicit_data_scope(SCOPE_RES_ATTENDANCE), None);

        claims
            .resource_scopes
            .insert(SCOPE_RES_ATTENDANCE.to_string(), "ALL".to_string());
        assert_eq!(claims.explicit_data_scope(SCOPE_RES_ATTENDANCE), Some(ScopeType::All));

        claims
            .resource_scopes
            .insert(SCOPE_RES_ATTENDANCE.to_string(), "INVALID".to_string());
        assert_eq!(claims.explicit_data_scope(SCOPE_RES_ATTENDANCE), None);
    }
}
