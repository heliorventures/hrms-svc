//! Request contexts injected by auth middleware.
//!
//! Two planes, two contexts. JWTs issued by the two planes MUST NOT be interchangeable
//! (different `iss` claim, different signing secret, validated by different middleware).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Data-level access control scope. Applied per resource per role via `PERMISSION_SCOPE`.
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
    /// Wider access wins when merging several role rows for the same resource.
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
    /// Widest `ScopeType` per `permission.resource` (keys: e.g. `employee`, `leave` — wire values
    /// are `SELF` | `TEAM` | `DEPARTMENT` | `ALL`). Omitted in legacy tokens; treated as SELF.
    #[serde(default)]
    pub resource_scopes: HashMap<String, String>,
}

pub const OPERATOR_JWT_ISSUER: &str = "kabipay-ops";
pub const CLIENT_JWT_ISSUER: &str = "kabipay-client";

/// JWT `permissions` claim uses `resource:action` to match `permission` rows.
pub const PERM_EMPLOYEE_WRITE: &str = "employee:write";
/// Broader org directory edits (e.g. bulk / sensitive fields) — same gate as write for now.
pub const PERM_EMPLOYEE_MANAGE: &str = "employee:manage";
/// Approve or reject other users' leave requests.
pub const PERM_LEAVE_APPROVE: &str = "leave:approve";
/// Approve or reject expense claims submitted by others.
pub const PERM_EXPENSE_APPROVE: &str = "expense:approve";
/// Configure expense categories (travel/meal/other claim types employees select).
pub const PERM_EXPENSE_MANAGE: &str = "expense:manage";
/// Mark expense reimbursements as paid / failed / on hold (payroll or accounting path).
pub const PERM_EXPENSE_PAY: &str = "expense:pay";
/// Approve or reject **tax proof** lines (submitted actuals vs declared deductions).
pub const PERM_TAX_PROOF_APPROVE: &str = "tax:approve";
/// Export India payroll statutory artefacts (e.g. monthly TDS summary CSV) for the tenant.
pub const PERM_PAYROLL_STATUTORY_EXPORT: &str = "payroll:statutory_export";
/// Configure live punch enforcement (geofence / IP allowlist) for the tenant.
pub const PERM_ATTENDANCE_PUNCH_POLICY: &str = "attendance:punch_policy";
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
/// Configure timesheet catalogs (projects / tasks) and lock policy (`master_data` backed).
pub const PERM_TIMESHEET_MANAGE: &str = "timesheet:manage";
/// Create or edit **tenant announcements**, send **direct in-app notifications**, and remove broadcasts.
pub const PERM_NOTIFICATION_MANAGE: &str = "notification:manage";

/// HTTP-derived metadata attached to each GraphQL request by [`crate::subgraph::tenant_graphql_post`].
/// Values come from gateway headers, not from GraphQL variables (so they are suitable for policy).
#[derive(Clone, Debug, Default)]
pub struct ClientRequestHints {
    /// First hop from `X-Forwarded-For`, else `X-Real-IP`, when present.
    pub client_ip: Option<String>,
}

impl ClientClaims {
    /// True if the token includes one of the permission strings (exact match on wire).
    pub fn has_any_permission(&self, perms: &[&str]) -> bool {
        perms
            .iter()
            .any(|p| self.permissions.iter().any(|owned| owned == p))
    }

    /// Create/update other users' **employee** rows (not self-service profile edits).
    pub fn can_manage_employee_directory(&self) -> bool {
        if self.has_any_permission(&[PERM_EMPLOYEE_WRITE, PERM_EMPLOYEE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Approve or reject **leave** requests (not the employee's own self-service only path).
    pub fn can_approve_leave(&self) -> bool {
        if self.has_any_permission(&[PERM_LEAVE_APPROVE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Approve or reject **expense** claims (approver/manager path).
    pub fn can_approve_expense(&self) -> bool {
        if self.has_any_permission(&[PERM_EXPENSE_APPROVE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Update expense **payment** / reimbursement status after approval.
    pub fn can_mark_expense_payment(&self) -> bool {
        if self.has_any_permission(&[PERM_EXPENSE_PAY]) {
            return true;
        }
        // Same baseline as approvers + accounting — finance often overlaps with expense approval.
        false
    }

    /// Approve or reject **tax deduction proof** lines (documented actuals).
    pub fn can_approve_tax_proof(&self) -> bool {
        if self.has_any_permission(&[PERM_TAX_PROOF_APPROVE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Download tenant-wide **statutory payroll** reports (TDS summary CSV, etc.).
    pub fn can_export_payroll_statutory(&self) -> bool {
        if self.has_any_permission(&[PERM_PAYROLL_STATUTORY_EXPORT]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Configure **live punch** policy (geofence + IP allowlist) for the tenant.
    pub fn can_configure_attendance_punch_policy(&self) -> bool {
        if self.has_any_permission(&[PERM_ATTENDANCE_PUNCH_POLICY]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Create or update **workflow** definitions and **steps** (tenant approval graphs).
    pub fn can_manage_workflow_definitions(&self) -> bool {
        if self.has_any_permission(&[PERM_WORKFLOW_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Configure leave types, policies, employee balances, and (via attendance) holiday calendars.
    pub fn can_manage_leave_configuration(&self) -> bool {
        if self.has_any_permission(&[PERM_LEAVE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Configure expense claim categories master data (`expense_category`).
    pub fn can_manage_expense_configuration(&self) -> bool {
        if self.has_any_permission(&[PERM_EXPENSE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Manage tenant RBAC: roles, permission grants, and list scopes (`role:manage` or elevated HR / tenant admin).
    pub fn can_manage_tenant_rbac(&self) -> bool {
        if self.has_any_permission(&[PERM_ROLE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_benefits_catalog(&self) -> bool {
        if self.has_any_permission(&[PERM_BENEFITS_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Benefit types/plans list queries for the workplace Benefits UI (HR + enrollment pickers).
    pub fn can_read_benefit_catalog_queries(&self) -> bool {
        if self.has_any_permission(&[PERM_BENEFITS_MANAGE, PERM_BENEFITS_SELF]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_recruitment(&self) -> bool {
        if self.has_any_permission(&[PERM_RECRUITMENT_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_performance_programs(&self) -> bool {
        if self.has_any_permission(&[PERM_PERFORMANCE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_learning_catalog(&self) -> bool {
        if self.has_any_permission(&[PERM_LEARNING_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_assets_registry(&self) -> bool {
        if self.has_any_permission(&[PERM_ASSETS_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_read_assets_registry(&self) -> bool {
        self.can_manage_assets_registry() || self.has_any_permission(&[PERM_ASSETS_READ])
    }

    pub fn can_read_own_assets(&self) -> bool {
        self.can_read_assets_registry() || self.has_any_permission(&[PERM_ASSETS_SELF])
    }

    pub fn can_manage_succession_planning(&self) -> bool {
        if self.has_any_permission(&[PERM_SUCCESSION_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_compensation_admin(&self) -> bool {
        if self.has_any_permission(&[PERM_COMPENSATION_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    pub fn can_manage_grievance_tenant_cases(&self) -> bool {
        if self.has_any_permission(&[PERM_GRIEVANCE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Submit grievances and view **own** cases (`grievance:self`, or manage, or legacy HR roles).
    pub fn can_use_grievance_self_service(&self) -> bool {
        if self.has_any_permission(&[PERM_GRIEVANCE_SELF, PERM_GRIEVANCE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Tenant-wide onboarding/offboarding lists and HR depth (`onboarding:manage`).
    pub fn can_manage_onboarding_tenant(&self) -> bool {
        if self.has_any_permission(&[PERM_ONBOARDING_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Join checklist + own separation flows (`onboarding:self`, or `onboarding:manage`, or legacy HR roles).
    pub fn can_use_onboarding_self_service(&self) -> bool {
        if self.has_any_permission(&[PERM_ONBOARDING_SELF, PERM_ONBOARDING_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Workforce insights UI (`analytics:read`) — dashboards, report catalog, snapshots.
    pub fn can_access_analytics_insights(&self) -> bool {
        if self.has_any_permission(&[PERM_ANALYTICS_READ]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Own-device punches require the explicit `attendance:punch_self` permission.
    pub fn can_record_own_attendance_punches(&self) -> bool {
        self.has_any_permission(&[PERM_ATTENDANCE_PUNCH_SELF])
    }

    /// Manual attendance corrections beyond the configured employee window (`attendance:regularize`).
    pub fn can_regularize_attendance_records(&self) -> bool {
        if self.has_any_permission(&[PERM_ATTENDANCE_REGULARIZE, PERM_EMPLOYEE_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Approve weekly timesheet batches (`timesheet:approve`), analogous to leave approval.
    pub fn can_approve_timesheet_requests(&self) -> bool {
        if self.has_any_permission(&[PERM_TIMESHEET_APPROVE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// HR configuration for timesheet projects/tasks and lock JSON (`timesheet:manage`).
    pub fn can_manage_timesheet_configuration(&self) -> bool {
        if self.has_any_permission(&[PERM_TIMESHEET_MANAGE]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// HR / comms admin: announcements, direct notifications, and related deletes. Also accepts
    /// split permissions from the RBAC catalog (`notification:*`).
    pub fn can_manage_notifications(&self) -> bool {
        if self.has_any_permission(&[
            PERM_NOTIFICATION_MANAGE,
            "notification:create",
            "notification:update",
            "notification:delete",
        ]) {
            return true;
        }
        self.roles.iter().any(|r| {
            let u = r.to_ascii_uppercase();
            u == "HR_ADMIN" || u == "TENANT_ADMIN" || u == "ORG_ADMIN"
        })
    }

    /// Effective data scope for list/detail filters (`permission_scope` merged at login). Defaults
    /// to `Self_` when unset (legacy tokens and least-privilege default).
    pub fn data_scope(&self, resource: &str) -> ScopeType {
        self.resource_scopes
            .get(resource)
            .and_then(|s| ScopeType::parse_loose(s))
            .unwrap_or(ScopeType::Self_)
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
            resource_scopes: HashMap::new(),
        }
    }

    #[test]
    fn self_punch_permission_grants_own_attendance_punch_access() {
        let claims = client_claims(&[], &[PERM_ATTENDANCE_PUNCH_SELF]);

        assert!(claims.can_record_own_attendance_punches());
    }

    #[test]
    fn administrator_role_does_not_replace_self_punch_permission() {
        let claims = client_claims(&["HR_ADMIN"], &[]);

        assert!(!claims.can_record_own_attendance_punches());
    }

    #[test]
    fn employee_directory_permission_does_not_replace_self_punch_permission() {
        let claims = client_claims(&[], &[PERM_EMPLOYEE_WRITE]);

        assert!(!claims.can_record_own_attendance_punches());
    }
}
