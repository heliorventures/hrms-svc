//! Versioned wire contracts. Deserialization alone never establishes authority.
use chrono::{DateTime, Utc};
pub use kabipay_organization_security::AuthorityKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, async_graphql::Enum)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationOperation {
    EmployeeLoginProvision,
    EmployeeList,
    EmployeeReferences,
    EmployeeDetail,
    EmployeeCreate,
    EmployeeUpdate,
    EmployeeEmployment,
    AccountList,
    AccountCreate,
    AccountUpdate,
    AccountAccess,
    AccountRoles,
    WorkflowList,
    WorkflowDetail,
    WorkflowSave,
    WorkflowActivation,
    SeparationList,
    SeparationDetail,
    SeparationCreate,
    SeparationReject,
    SeparationCancelApproval,
    ClearanceUpdate,
    PayrollJobList,
    PayrollJobDetail,
    PayrollJobCancelBeforeExecution,
    InstallMembership,
    InstallGrant,
    Directory,
    InstallRelationship,
    LeaveDetail,
    LeaveDecision,
    FeedbackCreate,
    ReviewDetail,
    ReviewSubmit,
    TimesheetDetail,
    TimesheetDecision,
    PayrollList,
    PayrollDetail,
    PayrollDraft,
    PayrollFinalize,
    ReadReceipt,
    CancelCommand,
}
impl OrganizationOperation {
    pub fn service(self) -> &'static str {
        match self {
            Self::EmployeeLoginProvision => "kabipay-employee",
            Self::EmployeeList => "kabipay-employee",
            Self::EmployeeReferences => "kabipay-employee",
            Self::EmployeeDetail => "kabipay-employee",
            Self::EmployeeCreate => "kabipay-employee",
            Self::EmployeeUpdate => "kabipay-employee",
            Self::EmployeeEmployment => "kabipay-employee",
            Self::AccountList => "kabipay-employee",
            Self::AccountCreate => "kabipay-employee",
            Self::AccountUpdate => "kabipay-employee",
            Self::AccountAccess => "kabipay-employee",
            Self::AccountRoles => "kabipay-employee",
            Self::WorkflowList => "kabipay-workflow",
            Self::WorkflowDetail => "kabipay-workflow",
            Self::WorkflowSave => "kabipay-workflow",
            Self::WorkflowActivation => "kabipay-workflow",
            Self::SeparationList => "kabipay-employee",
            Self::SeparationDetail => "kabipay-employee",
            Self::SeparationCreate => "kabipay-employee",
            Self::SeparationReject => "kabipay-employee",
            Self::SeparationCancelApproval => "kabipay-employee",
            Self::ClearanceUpdate => "kabipay-employee",
            Self::PayrollJobList => "kabipay-payroll",
            Self::PayrollJobDetail => "kabipay-payroll",
            Self::PayrollJobCancelBeforeExecution => "kabipay-payroll",
            Self::InstallMembership => "kabipay-employee",
            Self::InstallGrant | Self::Directory | Self::InstallRelationship => "kabipay-employee",
            Self::LeaveDetail | Self::LeaveDecision => "kabipay-leave",
            Self::FeedbackCreate | Self::ReviewDetail | Self::ReviewSubmit => "kabipay-performance",
            Self::TimesheetDetail | Self::TimesheetDecision => "kabipay-attendance",
            Self::PayrollList
            | Self::PayrollDetail
            | Self::PayrollDraft
            | Self::PayrollFinalize => "kabipay-payroll",
            Self::ReadReceipt | Self::CancelCommand => "receipt",
        }
    }
    pub fn capability(self) -> &'static str {
        match self {
            Self::EmployeeLoginProvision => "role:manage",
            Self::EmployeeList => "employee:manage",
            Self::EmployeeReferences => "employee:manage",
            Self::EmployeeDetail => "employee:manage",
            Self::EmployeeCreate => "employee:manage",
            Self::EmployeeUpdate => "employee:manage",
            Self::EmployeeEmployment => "employee:manage",
            Self::AccountList => "role:manage",
            Self::AccountCreate => "role:manage",
            Self::AccountUpdate => "role:manage",
            Self::AccountAccess => "role:manage",
            Self::AccountRoles => "role:manage",
            Self::WorkflowList => "workflow:manage",
            Self::WorkflowDetail => "workflow:manage",
            Self::WorkflowSave => "workflow:manage",
            Self::WorkflowActivation => "workflow:manage",
            Self::SeparationList => "onboarding:manage",
            Self::SeparationDetail => "onboarding:manage",
            Self::SeparationCreate => "onboarding:manage",
            Self::SeparationReject => "onboarding:manage",
            Self::SeparationCancelApproval => "onboarding:manage",
            Self::ClearanceUpdate => "onboarding:manage",
            Self::PayrollJobList => "payroll:manage",
            Self::PayrollJobDetail => "payroll:manage",
            Self::PayrollJobCancelBeforeExecution => "payroll:manage",
            Self::InstallMembership => "organization:manage",
            Self::InstallGrant => "organization:delegate",
            Self::Directory | Self::InstallRelationship => "organization:manage",
            Self::LeaveDetail | Self::LeaveDecision => "leave:approve",
            Self::FeedbackCreate => "feedback:write",
            Self::ReviewDetail | Self::ReviewSubmit => "performance:evaluate",
            Self::TimesheetDetail | Self::TimesheetDecision => "timesheet:approve",
            Self::PayrollList
            | Self::PayrollDetail
            | Self::PayrollDraft
            | Self::PayrollFinalize => "payroll:manage",
            Self::ReadReceipt | Self::CancelCommand => "organization_command:read",
        }
    }
    pub fn resource_type(self) -> &'static str {
        match self {
            Self::EmployeeLoginProvision => "employee",
            Self::EmployeeList => "company_employees",
            Self::EmployeeReferences => "employee_references",
            Self::EmployeeDetail => "employee",
            Self::EmployeeCreate => "employee",
            Self::EmployeeUpdate => "employee",
            Self::EmployeeEmployment => "employee",
            Self::AccountList => "company_accounts",
            Self::AccountCreate => "user",
            Self::AccountUpdate => "user",
            Self::AccountAccess => "user",
            Self::AccountRoles => "user",
            Self::WorkflowList => "company_workflows",
            Self::WorkflowDetail => "workflow_definition",
            Self::WorkflowSave => "workflow_definition",
            Self::WorkflowActivation => "workflow_definition",
            Self::SeparationList => "company_separations",
            Self::SeparationDetail => "separation",
            Self::SeparationCreate => "separation",
            Self::SeparationReject => "separation",
            Self::SeparationCancelApproval => "separation",
            Self::ClearanceUpdate => "clearance_checklist",
            Self::PayrollJobList => "payroll_job",
            Self::PayrollJobDetail => "payroll_job",
            Self::PayrollJobCancelBeforeExecution => "payroll_job",
            Self::InstallMembership => "organization_membership",
            Self::InstallGrant => "organization_grant",
            Self::Directory => "company_directory",
            Self::InstallRelationship => "management_assignment",
            Self::LeaveDetail | Self::LeaveDecision => "leave_request",
            Self::FeedbackCreate => "employee_feedback",
            Self::ReviewDetail | Self::ReviewSubmit => "performance_participant",
            Self::TimesheetDetail | Self::TimesheetDecision => "timesheet",
            Self::PayrollList
            | Self::PayrollDetail
            | Self::PayrollDraft
            | Self::PayrollFinalize => "payroll_run",
            Self::ReadReceipt | Self::CancelCommand => "organization_command",
        }
    }
    pub fn is_company_administration(self) -> bool {
        matches!(
            self,
            Self::EmployeeLoginProvision
                | Self::EmployeeList
                | Self::EmployeeReferences
                | Self::EmployeeDetail
                | Self::EmployeeCreate
                | Self::EmployeeUpdate
                | Self::EmployeeEmployment
                | Self::AccountList
                | Self::AccountCreate
                | Self::AccountUpdate
                | Self::AccountAccess
                | Self::AccountRoles
                | Self::WorkflowList
                | Self::WorkflowDetail
                | Self::WorkflowSave
                | Self::WorkflowActivation
                | Self::SeparationList
                | Self::SeparationDetail
                | Self::SeparationCreate
                | Self::SeparationReject
                | Self::SeparationCancelApproval
                | Self::ClearanceUpdate
        )
    }
    pub fn is_company_collection(self) -> bool {
        matches!(
            self,
            Self::EmployeeList
                | Self::EmployeeReferences
                | Self::AccountList
                | Self::WorkflowList
                | Self::SeparationList
                | Self::PayrollList
                | Self::PayrollJobList
        )
    }
    pub fn may_create_resource(self) -> bool {
        matches!(
            self,
            Self::EmployeeCreate
                | Self::AccountCreate
                | Self::SeparationCreate
                | Self::WorkflowSave
        )
    }
    pub fn is_read(self) -> bool {
        matches!(
            self,
            Self::EmployeeList
                | Self::EmployeeReferences
                | Self::EmployeeDetail
                | Self::AccountList
                | Self::WorkflowList
                | Self::WorkflowDetail
                | Self::SeparationList
                | Self::SeparationDetail
                | Self::PayrollJobList
                | Self::PayrollJobDetail
                | Self::Directory
                | Self::LeaveDetail
                | Self::ReviewDetail
                | Self::TimesheetDetail
                | Self::PayrollList
                | Self::ReadReceipt
                | Self::PayrollDetail
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizationCommand {
    pub command_id: Uuid,
    pub resource_id: Uuid,
    pub expected_version: i64,
    pub operation: OrganizationOperation,
    pub payload: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizationCommandOutcome {
    pub command_id: Uuid,
    pub resource_id: Uuid,
    pub status: String,
    pub resource_version: i64,
    pub result: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePolicyRequest {
    pub delegation_token: String,
    pub command_body_base64: String,
    pub employee_id: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorReference {
    pub principal_id: Uuid,
    pub display_name: String,
    pub identity_version: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledCapability {
    pub code: String,
    pub employee_scope: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantInstallation {
    pub organization_id: Uuid,
    pub membership_id: Uuid,
    pub tenant_id: Uuid,
    pub binding_id: Uuid,
    pub principal_id: Uuid,
    pub grant_version: i64,
    pub membership_version: i64,
    pub capabilities: Vec<InstalledCapability>,
    pub actor: ActorReference,
    pub configuration_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipInstallation {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub membership_id: Uuid,
    pub tenant_id: Uuid,
    pub subject_employee_id: Uuid,
    pub subject_principal_id: Uuid,
    pub manager_principal_id: Uuid,
    pub subject_employment_ref_id: Uuid,
    pub manager_employment_ref_id: Uuid,
    pub manager_tenant_id: Uuid,
    pub manager_employee_id: Option<Uuid>,
    pub relationship_type: String,
    pub status: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub version: i64,
    pub graph_version: i64,
    pub capabilities: Vec<String>,
    pub actors: Vec<ActorReference>,
    pub configuration_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAuthorization {
    pub organization_id: Uuid,
    pub membership_id: Uuid,
    pub tenant_id: Uuid,
    pub principal_id: Uuid,
    pub source_tenant_id: Uuid,
    pub source_user_id: Uuid,
    pub identity_version: i64,
    pub source_link_version: i64,
    pub membership_version: i64,
    pub authority_kind: AuthorityKind,
    pub authority_id: Uuid,
    pub authority_version: i64,
    pub capability: String,
    pub employee_scope: String,
    pub employee_id: Option<Uuid>,
    #[serde(default)]
    pub scope_employee_id: Option<Uuid>,
    #[serde(default)]
    pub actor_target_user_ids: Vec<Uuid>,
    pub actor: ActorReference,
    pub grant: Option<GrantInstallation>,
    pub relationship: Option<RelationshipInstallation>,
    #[serde(default)]
    pub membership: Option<MembershipInstallation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipInstallation {
    pub organization_id: Uuid,
    pub membership_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub version: i64,
    pub configuration_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipAcknowledgement {
    pub membership_id: Uuid,
    pub status: String,
    pub version: i64,
    pub configuration_hash: String,
    pub command_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantAcknowledgement {
    pub membership_id: Uuid,
    pub binding_id: Uuid,
    pub grant_version: i64,
    pub configuration_hash: String,
    pub command_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipAcknowledgement {
    pub membership_id: Uuid,
    pub assignment_id: Uuid,
    pub version: i64,
    pub graph_version: i64,
    pub configuration_hash: String,
    pub command_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEligibilityRequest {
    pub user_id: Uuid,
    pub nonce: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEligibilityResponse {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub nonce: Uuid,
    pub eligible: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmploymentEligibilityRequest {
    pub employee_id: Uuid,
    pub nonce: Uuid,
    pub expected_user_id: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmploymentEligibilityResponse {
    pub employee_id: Uuid,
    pub tenant_id: Uuid,
    pub nonce: Uuid,
    pub eligible: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyEvent {
    pub schema_version: i32,
    pub event_id: Uuid,
    pub event_type: String,
    pub organization_id: Uuid,
    pub source_tenant_id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    pub occurred_at: DateTime<Utc>,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<CompanyEventActor>,
    pub payload: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompanyEventActor {
    OrganizationPrincipal {
        principal_id: Uuid,
        display_name: String,
        identity_version: i64,
        source_tenant_id: Uuid,
        source_user_id: Uuid,
        authority_kind: AuthorityKind,
        authority_id: Uuid,
        authority_version: i64,
        command_id: Uuid,
    },
    LocalUser {
        local_user_id: Uuid,
        source_tenant_id: Uuid,
    },
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayrollJobAuthorizationRequest {
    pub original_command_body_base64: String,
    pub original_authorization: RuntimeAuthorization,
    pub nonce: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayrollJobAuthorizationResponse {
    pub delegation_token: String,
    pub nonce: Uuid,
}

#[cfg(test)]
mod operation_gap_tests {
    use super::*;
    #[test]
    fn administrative_and_job_operations_have_explicit_protected_routes() {
        for (code, service, capability, resource, read) in [
            (
                "employee_detail",
                "kabipay-employee",
                "employee:manage",
                "employee",
                true,
            ),
            (
                "employee_create",
                "kabipay-employee",
                "employee:manage",
                "employee",
                false,
            ),
            (
                "account_list",
                "kabipay-employee",
                "role:manage",
                "company_accounts",
                true,
            ),
            (
                "account_access",
                "kabipay-employee",
                "role:manage",
                "user",
                false,
            ),
            (
                "workflow_save",
                "kabipay-workflow",
                "workflow:manage",
                "workflow_definition",
                false,
            ),
            (
                "workflow_list",
                "kabipay-workflow",
                "workflow:manage",
                "company_workflows",
                true,
            ),
            (
                "payroll_job_detail",
                "kabipay-payroll",
                "payroll:manage",
                "payroll_job",
                true,
            ),
            (
                "payroll_job_cancel_before_execution",
                "kabipay-payroll",
                "payroll:manage",
                "payroll_job",
                false,
            ),
        ] {
            let operation: OrganizationOperation =
                serde_json::from_value(serde_json::json!(code)).unwrap();
            assert_eq!(operation.service(), service);
            assert_eq!(operation.capability(), capability);
            assert_eq!(operation.resource_type(), resource);
            assert_eq!(operation.is_read(), read);
        }
    }
}
