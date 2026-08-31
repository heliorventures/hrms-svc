//! GraphQL DTOs for kabipay-payroll.

use async_graphql::{InputObject, SimpleObject, ID};
use kabipay_db_entities::tenant::d0035_payroll_arrear::payroll_arrear;
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_db_entities::tenant::d0012_payroll::{
    employee_salary_structure, payroll_compliance_setting, payroll_cycle, payslip,
    payslip_component, salary_component, salary_structure, salary_structure_component,
};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SalaryComponent")]
pub struct SalaryComponentDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: String,
    pub component_type: String,
    pub is_taxable: bool,
    pub is_fixed: bool,
    pub is_active: bool,
    pub formula_expression: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<salary_component::Model> for SalaryComponentDto {
    fn from(m: salary_component::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            component_type: m.r#type,
            is_taxable: m.is_taxable,
            is_fixed: m.is_fixed,
            is_active: m.is_active,
            formula_expression: m.formula_expression,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SalaryStructureComponent")]
pub struct SalaryStructureComponentDto {
    pub id: ID,
    pub salary_component_id: ID,
    pub component_name: String,
    pub component_code: String,
    pub component_type: String,
    pub calculation_basis: String,
    pub calculation_value: Option<String>,
    pub display_order: i32,
}

impl SalaryStructureComponentDto {
    pub fn from_parts(m: salary_structure_component::Model, component: salary_component::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            salary_component_id: ID(m.salary_component_id.to_string()),
            component_name: component.name,
            component_code: component.code,
            component_type: component.r#type,
            calculation_basis: m.calculation_basis,
            calculation_value: m
                .calculation_value
                .or(m.percentage_of_basic)
                .or(m.amount)
                .map(|d| d.to_string()),
            display_order: m.display_order,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SalaryStructure")]
pub struct SalaryStructureDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub description: Option<String>,
    pub components: Vec<SalaryStructureComponentDto>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SalaryStructureDto {
    pub fn from_head(m: salary_structure::Model, components: Vec<SalaryStructureComponentDto>) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            description: m.description,
            components,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "EmployeeSalaryStructure")]
pub struct EmployeeSalaryStructureDto {
    pub id: ID,
    pub employee_id: ID,
    pub salary_structure_id: ID,
    pub ctc: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<employee_salary_structure::Model> for EmployeeSalaryStructureDto {
    fn from(m: employee_salary_structure::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            salary_structure_id: ID(m.salary_structure_id.to_string()),
            ctc: m.ctc.to_string(),
            effective_from: m.effective_from,
            effective_to: m.effective_to,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SalaryBreakupLine")]
pub struct SalaryBreakupLineDto {
    pub salary_component_id: ID,
    pub component_name: String,
    pub component_code: String,
    pub component_type: String,
    pub calculation_basis: String,
    pub calculation_value: String,
    pub annual_amount: String,
    pub monthly_amount: String,
    pub is_override: bool,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SalaryBreakupPreview")]
pub struct SalaryBreakupPreviewDto {
    pub employee_id: ID,
    pub employee_salary_structure_id: Option<ID>,
    pub annual_ctc: String,
    pub monthly_gross: String,
    pub monthly_deductions: String,
    pub monthly_net_before_statutory: String,
    pub lines: Vec<SalaryBreakupLineDto>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PayrollCycle")]
pub struct PayrollCycleDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub month: i32,
    pub year: i32,
    pub status: String,
    pub payment_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PayslipComponentLine")]
pub struct PayslipComponentLineDto {
    pub id: ID,
    pub tenant_id: ID,
    pub payslip_id: ID,
    pub salary_component_id: ID,
    /// Decimal as string
    pub amount: String,
    pub component_type: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<payslip_component::Model> for PayslipComponentLineDto {
    fn from(m: payslip_component::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            payslip_id: ID(m.payslip_id.to_string()),
            salary_component_id: ID(m.salary_component_id.to_string()),
            amount: m.amount.to_string(),
            component_type: m.component_type,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Payslip")]
pub struct PayslipDetailDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub payroll_cycle_id: ID,
    pub period_month: i32,
    pub period_year: i32,
    pub gross_salary: String,
    pub total_deductions: String,
    pub net_salary: String,
    pub pf_employee: Option<String>,
    pub pf_employer: Option<String>,
    pub esi_employee: Option<String>,
    pub esi_employer: Option<String>,
    pub tds_amount: Option<String>,
    pub professional_tax: Option<String>,
    pub uan_number: Option<String>,
    pub esic_number: Option<String>,
    pub status: String,
    pub generated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub lines: Vec<PayslipComponentLineDto>,
}

impl PayslipDetailDto {
    pub fn from_head(
        m: payslip::Model,
        cycle: &payroll_cycle::Model,
        lines: Vec<payslip_component::Model>,
    ) -> Self {
        let lines = lines
            .into_iter()
            .map(PayslipComponentLineDto::from)
            .collect();
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            payroll_cycle_id: ID(m.payroll_cycle_id.to_string()),
            period_month: cycle.month,
            period_year: cycle.year,
            gross_salary: m.gross_salary.to_string(),
            total_deductions: m.total_deductions.to_string(),
            net_salary: m.net_salary.to_string(),
            pf_employee: m.pf_employee.map(|d| d.to_string()),
            pf_employer: m.pf_employer.map(|d| d.to_string()),
            esi_employee: m.esi_employee.map(|d| d.to_string()),
            esi_employer: m.esi_employer.map(|d| d.to_string()),
            tds_amount: m.tds_amount.map(|d| d.to_string()),
            professional_tax: m.professional_tax.map(|d| d.to_string()),
            uan_number: m.uan_number,
            esic_number: m.esic_number,
            status: m.status,
            generated_at: m.generated_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            lines,
        }
    }
}

impl From<payroll_cycle::Model> for PayrollCycleDto {
    fn from(m: payroll_cycle::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            month: m.month,
            year: m.year,
            status: m.status,
            payment_date: m.payment_date,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// PENDING or APPLIED back-pay / correction accrual; applied on a pay run as an `ARREAR` line.
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PayrollArrear")]
pub struct PayrollArrearDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    /// Decimal as string
    pub amount: String,
    pub reason: Option<String>,
    pub status: String,
    pub applied_payroll_cycle_id: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<payroll_arrear::Model> for PayrollArrearDto {
    fn from(m: payroll_arrear::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            amount: m.amount.to_string(),
            reason: m.reason,
            status: m.status,
            applied_payroll_cycle_id: m
                .applied_payroll_cycle_id
                .map(|u| ID(u.to_string())),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Create a `PENDING` arrear; paid out on the next pay run that includes the employee.
#[derive(InputObject, Clone, Debug)]
pub struct CreatePayrollArrearInput {
    pub employee_id: ID,
    /// Decimal string, e.g. "5000.00"
    pub amount: String,
    pub reason: Option<String>,
}

/// Tenant payroll presentation + statutory CSV placeholders (India Form 16 / 24Q prep).
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PayrollComplianceSetting")]
pub struct PayrollComplianceSettingDto {
    pub employer_tan: Option<String>,
    pub employer_legal_name: Option<String>,
    /// Salary `salary_component.code` used as the employment **base** line on pay run (`EARNING`).
    pub base_salary_component_code: String,
    /// Salary component code used for **`ARREAR`** payout lines (`EARNING`).
    pub arrear_salary_component_code: String,
    /// Heading text on payslip when rendered (e.g. company display name).
    pub payslip_header_title: Option<String>,
    /// Uploaded logo in **`file_storage`** (tenant-scoped blob); optional.
    pub payslip_logo_file_storage_id: Option<ID>,
}

impl From<payroll_compliance_setting::Model> for PayrollComplianceSettingDto {
    fn from(m: payroll_compliance_setting::Model) -> Self {
        Self {
            employer_tan: m.employer_tan,
            employer_legal_name: m.employer_legal_name,
            base_salary_component_code: m.base_salary_component_code,
            arrear_salary_component_code: m.arrear_salary_component_code,
            payslip_header_title: m.payslip_header_title,
            payslip_logo_file_storage_id: m
                .payslip_logo_file_storage_id
                .map(|u| ID(u.to_string())),
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertPayrollComplianceSettingInput {
    pub employer_tan: Option<String>,
    pub employer_legal_name: Option<String>,
    pub base_salary_component_code: Option<String>,
    pub arrear_salary_component_code: Option<String>,
    pub payslip_header_title: Option<String>,
    pub payslip_logo_file_storage_id: Option<ID>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertSalaryComponentInput {
    pub id: Option<ID>,
    pub name: String,
    pub code: String,
    pub component_type: String,
    pub is_taxable: bool,
    pub is_fixed: bool,
    pub is_active: bool,
    pub formula_expression: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct SalaryStructureComponentInput {
    pub salary_component_id: ID,
    pub calculation_basis: String,
    pub calculation_value: String,
    pub display_order: i32,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertSalaryStructureInput {
    pub id: Option<ID>,
    pub name: String,
    pub description: Option<String>,
    pub components: Vec<SalaryStructureComponentInput>,
}

#[derive(InputObject, Clone, Debug)]
pub struct EmployeeSalaryComponentOverrideInput {
    pub salary_component_id: ID,
    pub calculation_basis: String,
    pub calculation_value: String,
    pub notes: Option<String>,
    pub is_active: bool,
}

#[derive(InputObject, Clone, Debug)]
pub struct AssignEmployeeSalaryStructureInput {
    pub employee_id: ID,
    pub salary_structure_id: ID,
    pub annual_ctc: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub overrides: Vec<EmployeeSalaryComponentOverrideInput>,
}

/// Create a new tenant payroll period row (`DRAFT`). One cycle per (tenant, month, year) in v1.
#[derive(InputObject, Clone, Debug)]
pub struct CreatePayrollCycleInput {
    /// Display label, e.g. "April 2026 payroll"
    pub name: String,
    /// Calendar month 1–12
    pub month: i32,
    pub year: i32,
    /// Optional pay-out date
    pub payment_date: Option<NaiveDate>,
}
