//! GraphQL output types for kabipay-employee.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use crate::entities::d0006_org_hierarchy::{department, designation};
use crate::entities::d0007_employee_core::{
    employee, employee_aadhaar, employee_bank, employee_pan, employment_history,
};
use crate::entities::d0008_document_system::{document_type, employee_document};
use crate::entities::d0017_onboarding_offboarding::{
    clearance_checklist, fnf_settlement, onboarding_checklist, separation,
};
use crate::entities::d0029_file_storage::file_storage;
use kabipay_db_entities::tenant::d0005_auth_rbac::{permission, permission_scope, role, user};

/// Federated `Employee` type. `id` is the canonical cross-service identifier (Gap A).
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Employee")]
pub struct EmployeeDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_code: String,
    pub first_name: String,
    pub last_name: String,
    /// Computed convenience field: `first_name` + space + `last_name`.
    pub full_name: String,
    pub status: String,
    pub employment_type: Option<String>,
    pub date_of_joining: NaiveDate,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    pub reporting_manager_id: Option<ID>,
    pub user_id: Option<ID>,
    #[graphql(name = "dateOfBirth")]
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub nationality: Option<String>,
    #[graphql(name = "emergencyContactName")]
    pub emergency_contact_name: Option<String>,
    #[graphql(name = "emergencyContactPhone")]
    pub emergency_contact_phone: Option<String>,
    #[graphql(name = "emergencyContactRelation")]
    pub emergency_contact_relation: Option<String>,
    #[graphql(name = "bloodGroup")]
    pub blood_group: Option<String>,
    /// Department display name when `department_id` is set (batch-resolved for directory queries).
    #[graphql(name = "departmentName")]
    pub department_name: Option<String>,
    #[graphql(name = "designationTitle")]
    pub designation_title: Option<String>,
    /// Linked login email when `user_id` is set.
    #[graphql(name = "linkedUserEmail")]
    pub linked_user_email: Option<String>,
    /// Linked login username when `user_id` is set. This is the sign-in identifier.
    #[graphql(name = "linkedUserUsername")]
    pub linked_user_username: Option<String>,
    #[graphql(name = "reportingManagerName")]
    pub reporting_manager_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "DocumentType")]
pub struct DocumentTypeDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub category: Option<String>,
    pub is_required: bool,
    pub expiry_alert_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<document_type::Model> for DocumentTypeDto {
    fn from(m: document_type::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            category: m.category,
            is_required: m.is_required,
            expiry_alert_days: m.expiry_alert_days,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

fn mask_pan_static(pan: &str) -> String {
    let t = pan.trim().to_uppercase().replace(' ', "");
    if t.len() < 5 {
        return "••••".to_string();
    }
    format!("{}••••{}", &t[..2], &t[t.len().saturating_sub(4)..])
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeeBankAccount")]
pub struct EmployeeBankAccountDto {
    pub id: ID,
    #[graphql(name = "bankName")]
    pub bank_name: String,
    #[graphql(name = "accountNumberMasked")]
    pub account_number_masked: String,
    #[graphql(name = "ifscCode")]
    pub ifsc_code: String,
    #[graphql(name = "accountType")]
    pub account_type: Option<String>,
    #[graphql(name = "isVerified")]
    pub is_verified: bool,
}

impl EmployeeBankAccountDto {
    pub fn from_model(m: &employee_bank::Model) -> Self {
        let digits: String = m
            .account_number
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let masked = if digits.len() >= 4 {
            format!("••••{}", &digits[digits.len().saturating_sub(4)..])
        } else {
            "••••".to_string()
        };
        Self {
            id: ID(m.id.to_string()),
            bank_name: m.bank_name.clone(),
            account_number_masked: masked,
            ifsc_code: m.ifsc_code.clone(),
            account_type: m.account_type.clone(),
            is_verified: m.is_verified,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeePanRecord")]
pub struct EmployeePanRecordDto {
    pub id: ID,
    #[graphql(name = "maskedPan")]
    pub masked_pan: String,
    #[graphql(name = "isVerified")]
    pub is_verified: bool,
}

impl EmployeePanRecordDto {
    pub fn from_model(m: &employee_pan::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            masked_pan: mask_pan_static(&m.pan_number),
            is_verified: m.is_verified,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeeAadhaarRecord")]
pub struct EmployeeAadhaarRecordDto {
    pub id: ID,
    #[graphql(name = "maskedAadhaar")]
    pub masked_aadhaar: String,
    #[graphql(name = "isVerified")]
    pub is_verified: bool,
}

impl EmployeeAadhaarRecordDto {
    pub fn from_model(m: &employee_aadhaar::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            masked_aadhaar: format!("XXXX XXXX {}", m.aadhaar_last4.trim()),
            is_verified: m.is_verified,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeeIdentityProfile")]
pub struct EmployeeIdentityProfileDto {
    pub pan: Option<EmployeePanRecordDto>,
    pub aadhaar: Option<EmployeeAadhaarRecordDto>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeeDocument")]
pub struct EmployeeDocumentDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub document_type_id: ID,
    pub status: String,
    pub expiry_date: Option<NaiveDate>,
    pub uploaded_at: DateTime<Utc>,
    #[graphql(name = "originalFileName")]
    pub original_file_name: Option<String>,
    #[graphql(name = "uploadedByUserId")]
    pub uploaded_by_user_id: Option<ID>,
    #[graphql(name = "documentTypeName")]
    pub document_type_name: Option<String>,
    #[graphql(name = "documentTypeCategory")]
    pub document_type_category: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<employee_document::Model> for EmployeeDocumentDto {
    fn from(m: employee_document::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            document_type_id: ID(m.document_type_id.to_string()),
            status: m.status,
            expiry_date: m.expiry_date,
            uploaded_at: m.uploaded_at,
            original_file_name: None,
            uploaded_by_user_id: None,
            document_type_name: None,
            document_type_category: None,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl EmployeeDocumentDto {
    pub fn with_file_and_type(
        mut self,
        original_file_name: Option<String>,
        uploaded_by_user_id: Option<Uuid>,
        document_type_name: Option<String>,
        document_type_category: Option<String>,
    ) -> Self {
        self.original_file_name = original_file_name;
        self.uploaded_by_user_id = uploaded_by_user_id.map(|u| ID(u.to_string()));
        self.document_type_name = document_type_name;
        self.document_type_category = document_type_category;
        self
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "OnboardingChecklistItem")]
pub struct OnboardingChecklistItemDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub task_name: String,
    pub task_category: Option<String>,
    pub assigned_to: Option<ID>,
    pub is_completed: bool,
    pub due_date: Option<NaiveDate>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<onboarding_checklist::Model> for OnboardingChecklistItemDto {
    fn from(m: onboarding_checklist::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            task_name: m.task_name,
            task_category: m.task_category,
            assigned_to: m.assigned_to.map(|u| ID(u.to_string())),
            is_completed: m.is_completed,
            due_date: m.due_date,
            completed_at: m.completed_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Department")]
pub struct DepartmentDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: String,
    pub parent_department_id: Option<ID>,
}

impl From<department::Model> for DepartmentDto {
    fn from(m: department::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            parent_department_id: m.parent_department_id.map(|u| ID(u.to_string())),
        }
    }
}

/// Flat reporting-line row; clients build a tree from `reporting_manager_id`.
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "OrgChartRow")]
pub struct OrgChartRowDto {
    pub employee_id: ID,
    pub employee_code: String,
    pub full_name: String,
    pub reporting_manager_id: Option<ID>,
    pub department_name: Option<String>,
    pub designation_title: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Designation")]
pub struct DesignationDto {
    pub id: ID,
    pub tenant_id: ID,
    pub department_id: ID,
    pub title: String,
    pub level: Option<String>,
    pub grade: Option<i32>,
}

impl From<designation::Model> for DesignationDto {
    fn from(m: designation::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            department_id: ID(m.department_id.to_string()),
            title: m.title,
            level: m.level,
            grade: m.grade,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct CreateEmployeeInput {
    pub employee_code: String,
    pub first_name: String,
    pub last_name: String,
    pub date_of_joining: NaiveDate,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    /// Must be another active employee in the tenant; cannot be self (enforced after id is chosen).
    pub reporting_manager_id: Option<ID>,
    pub employment_type: Option<String>,
    /// Defaults to `ACTIVE` when omitted.
    pub status: Option<String>,
    pub user_id: Option<ID>,
    #[graphql(name = "loginAccount")]
    pub login_account: Option<EmployeeLoginAccountInput>,
}

#[derive(InputObject, Clone, Debug)]
pub struct EmployeeLoginAccountInput {
    pub username: String,
    pub email: Option<String>,
    #[graphql(name = "initialPassword")]
    pub initial_password: String,
    #[graphql(name = "roleIds")]
    pub role_ids: Option<Vec<ID>>,
}

#[derive(InputObject, Clone, Debug)]
pub struct ProvisionEmployeeLoginInput {
    #[graphql(name = "employeeId")]
    pub employee_id: ID,
    pub username: String,
    pub email: Option<String>,
    #[graphql(name = "initialPassword")]
    pub initial_password: String,
    #[graphql(name = "roleIds")]
    pub role_ids: Option<Vec<ID>>,
}

#[derive(InputObject, Clone, Debug)]
pub struct ResetEmployeePasswordInput {
    #[graphql(name = "employeeId")]
    pub employee_id: ID,
    #[graphql(name = "newPassword")]
    pub new_password: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateEmployeeInput {
    pub id: ID,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    /// Omitted = leave unchanged; `null` = clear manager; id = set manager (cycle-safe).
    pub reporting_manager_id: Option<Option<ID>>,
    pub employment_type: Option<String>,
    pub status: Option<String>,
    pub user_id: Option<ID>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateEmployeePersonalProfileInput {
    pub employee_id: ID,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub nationality: Option<String>,
    #[graphql(name = "bloodGroup")]
    pub blood_group: Option<String>,
    pub emergency_contact_name: Option<String>,
    pub emergency_contact_phone: Option<String>,
    pub emergency_contact_relation: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertEmployeePrimaryBankInput {
    pub employee_id: ID,
    pub bank_name: String,
    pub account_number: String,
    pub ifsc_code: String,
    pub account_type: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertEmployeePrimaryPanInput {
    pub employee_id: ID,
    /// 10-character Indian PAN (letters + digits), case-insensitive.
    #[graphql(name = "panNumber")]
    pub pan_number: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertEmployeePrimaryAadhaarInput {
    pub employee_id: ID,
    /// Last 4 digits, or full 12-digit number (spaces allowed); only last 4 are stored.
    #[graphql(name = "aadhaarNumber")]
    pub aadhaar_number: String,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmploymentHistoryRecord")]
pub struct EmploymentHistoryRecordDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    /// Monthly amount used as base gross for pay run (maps to `employment_history.salary`).
    pub monthly_salary: Option<String>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub change_reason: Option<String>,
    pub changed_by: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(InputObject, Clone, Debug)]
pub struct SetEmployeeCompensationInput {
    pub employee_id: ID,
    /// Monthly gross (BASIC) for payroll — must match Decimal string (e.g. `65000` or `65000.00`).
    pub monthly_salary: String,
    pub effective_from: NaiveDate,
    pub change_reason: Option<String>,
}

impl From<employment_history::Model> for EmploymentHistoryRecordDto {
    fn from(m: employment_history::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            monthly_salary: m.salary.map(|d| d.to_string()),
            effective_from: m.effective_from,
            effective_to: m.effective_to,
            change_reason: m.change_reason,
            changed_by: m.changed_by.map(|u| ID(u.to_string())),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Separation")]
pub struct SeparationDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub separation_type: String,
    pub resignation_date: Option<NaiveDate>,
    pub last_working_date: NaiveDate,
    pub reason: Option<String>,
    pub status: String,
    pub approved_by: Option<ID>,
    pub workflow_instance_id: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<separation::Model> for SeparationDto {
    fn from(m: separation::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            separation_type: m.separation_type,
            resignation_date: m.resignation_date,
            last_working_date: m.last_working_date,
            reason: m.reason,
            status: m.status,
            approved_by: m.approved_by.map(|u| ID(u.to_string())),
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "FnfSettlement")]
pub struct FnfSettlementDto {
    pub id: ID,
    pub tenant_id: ID,
    pub separation_id: ID,
    pub leave_encashment: Option<String>,
    pub gratuity_amount: Option<String>,
    pub bonus_payable: Option<String>,
    pub recovery_amount: Option<String>,
    pub net_payable: Option<String>,
    pub status: String,
    pub processed_at: Option<DateTime<Utc>>,
    pub processed_by: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<fnf_settlement::Model> for FnfSettlementDto {
    fn from(m: fnf_settlement::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            separation_id: ID(m.separation_id.to_string()),
            leave_encashment: m.leave_encashment.map(|d| d.to_string()),
            gratuity_amount: m.gratuity_amount.map(|d| d.to_string()),
            bonus_payable: m.bonus_payable.map(|d| d.to_string()),
            recovery_amount: m.recovery_amount.map(|d| d.to_string()),
            net_payable: m.net_payable.map(|d| d.to_string()),
            status: m.status,
            processed_at: m.processed_at,
            processed_by: m.processed_by.map(|u| ID(u.to_string())),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ClearanceChecklistItem")]
pub struct ClearanceChecklistItemDto {
    pub id: ID,
    pub tenant_id: ID,
    pub separation_id: ID,
    pub department: String,
    pub task_name: String,
    pub is_cleared: bool,
    pub cleared_by: Option<ID>,
    pub cleared_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<clearance_checklist::Model> for ClearanceChecklistItemDto {
    fn from(m: clearance_checklist::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            separation_id: ID(m.separation_id.to_string()),
            department: m.department,
            task_name: m.task_name,
            is_cleared: m.is_cleared,
            cleared_by: m.cleared_by.map(|u| ID(u.to_string())),
            cleared_at: m.cleared_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct SubmitSeparationInput {
    /// When omitted, the JWT-linked employee is used (self-service exit request).
    pub employee_id: Option<ID>,
    pub separation_type: String,
    pub resignation_date: Option<NaiveDate>,
    pub last_working_date: NaiveDate,
    pub reason: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertFnfSettlementInput {
    pub separation_id: ID,
    /// Decimal as string, e.g. "12500.50". Omit or empty to clear.
    pub leave_encashment: Option<String>,
    pub gratuity_amount: Option<String>,
    pub bonus_payable: Option<String>,
    pub recovery_amount: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UploadEmployeeDocumentInput {
    pub employee_id: ID,
    pub document_type_id: ID,
    pub file_name: String,
    pub mime_type: Option<String>,
    /// Standard base64 (not data-URL). Max ~6MB decoded.
    pub content_base64: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UploadTenantFileInput {
    pub file_name: String,
    pub mime_type: Option<String>,
    /// Standard base64 (not data-URL). Max ~6MB decoded.
    pub content_base64: String,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "UploadedTenantFile")]
pub struct UploadedTenantFileDto {
    pub id: ID,
    pub tenant_id: ID,
    pub original_file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i32>,
    pub created_at: DateTime<Utc>,
}

impl From<file_storage::Model> for UploadedTenantFileDto {
    fn from(m: file_storage::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            original_file_name: m.original_filename,
            mime_type: m.mime_type,
            file_size_bytes: m.file_size_bytes.and_then(|size| i32::try_from(size).ok()),
            created_at: m.created_at,
        }
    }
}

impl From<employee::Model> for EmployeeDto {
    fn from(m: employee::Model) -> Self {
        let full_name = format!("{} {}", m.first_name.trim(), m.last_name.trim())
            .trim()
            .to_string();
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_code: m.employee_code,
            first_name: m.first_name,
            last_name: m.last_name,
            full_name,
            status: m.status,
            employment_type: m.employment_type,
            date_of_joining: m.date_of_joining,
            department_id: m.department_id.map(|id| ID(id.to_string())),
            designation_id: m.designation_id.map(|id| ID(id.to_string())),
            reporting_manager_id: m.reporting_manager_id.map(|id| ID(id.to_string())),
            user_id: m.user_id.map(|id| ID(id.to_string())),
            date_of_birth: m.date_of_birth,
            gender: m.gender,
            nationality: m.nationality,
            emergency_contact_name: m.emergency_contact_name,
            emergency_contact_phone: m.emergency_contact_phone,
            emergency_contact_relation: m.emergency_contact_relation,
            blood_group: m.blood_group,
            department_name: None,
            designation_title: None,
            linked_user_email: None,
            linked_user_username: None,
            reporting_manager_name: None,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl EmployeeDto {
    pub fn with_reference_labels(
        mut self,
        dept_map: &std::collections::HashMap<Uuid, String>,
        desig_map: &std::collections::HashMap<Uuid, String>,
        user_login_map: &std::collections::HashMap<Uuid, (String, Option<String>)>,
        mgr_name_map: &std::collections::HashMap<Uuid, String>,
    ) -> Self {
        fn opt_uuid(id: &Option<ID>) -> Option<Uuid> {
            id.as_ref()
                .and_then(|raw| Uuid::parse_str(raw.as_str()).ok())
        }
        self.department_name =
            opt_uuid(&self.department_id).and_then(|u| dept_map.get(&u).cloned());
        self.designation_title =
            opt_uuid(&self.designation_id).and_then(|u| desig_map.get(&u).cloned());
        if let Some((username, email)) =
            opt_uuid(&self.user_id).and_then(|u| user_login_map.get(&u))
        {
            self.linked_user_username = Some(username.clone());
            self.linked_user_email = email.clone();
        }
        self.reporting_manager_name =
            opt_uuid(&self.reporting_manager_id).and_then(|u| mgr_name_map.get(&u).cloned());
        self
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantDirectoryUser")]
pub struct TenantDirectoryUserDto {
    pub id: ID,
    pub username: String,
    pub email: Option<String>,
    pub is_active: bool,
}

impl From<user::Model> for TenantDirectoryUserDto {
    fn from(m: user::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            username: m.username,
            email: m.email,
            is_active: m.is_active,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantDirectoryRole")]
pub struct TenantDirectoryRoleDto {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub is_system_role: bool,
}

impl From<role::Model> for TenantDirectoryRoleDto {
    fn from(m: role::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            name: m.name,
            description: m.description,
            is_system_role: m.is_system_role,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantCatalogPermission")]
pub struct TenantCatalogPermissionDto {
    pub id: ID,
    pub resource: String,
    pub action: String,
    pub description: Option<String>,
}

impl From<permission::Model> for TenantCatalogPermissionDto {
    fn from(m: permission::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            resource: m.resource,
            action: m.action,
            description: m.description,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantPermissionScopeAssignment")]
pub struct TenantPermissionScopeDto {
    pub id: ID,
    pub resource: String,
    pub action: String,
    pub scope_type: String,
}

impl From<permission_scope::Model> for TenantPermissionScopeDto {
    fn from(m: permission_scope::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            resource: m.resource,
            action: m.action,
            scope_type: m.scope_type,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct PermissionScopeAssignmentInput {
    pub resource: String,
    pub action: String,
    pub scope_type: String,
}
