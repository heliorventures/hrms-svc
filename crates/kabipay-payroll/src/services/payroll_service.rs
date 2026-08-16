//! Tenant-scoped SeaORM queries for payroll catalog and cycles.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::{
    employee, employee_bank, employee_pan, employment_history,
};
use kabipay_db_entities::tenant::d0012_payroll::{
    employee_salary_component_override, employee_salary_structure, payroll_compliance_setting,
    payroll_cycle, payslip, payslip_component, salary_component, salary_structure,
    salary_structure_component,
};
use kabipay_db_entities::tenant::d0013_tax_statutory::tax_computation;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use uuid::Uuid;

use crate::services::arrear_service;
use crate::services::statutory_india;

pub async fn list_components(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    active_only: bool,
    limit: u64,
) -> KabiPayResult<Vec<salary_component::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q =
        salary_component::Entity::find().filter(salary_component::Column::TenantId.eq(tenant_id));
    if active_only {
        q = q.filter(salary_component::Column::IsActive.eq(true));
    }
    q.order_by_asc(salary_component::Column::Code)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

fn normalize_component_code(code: &str) -> KabiPayResult<String> {
    let normalized = code.trim().to_ascii_uppercase().replace(' ', "_");
    if normalized.is_empty() {
        return Err(KabiPayError::Validation("component code must not be empty".into()));
    }
    if normalized.len() > 50 {
        return Err(KabiPayError::Validation("component code must be 50 characters or less".into()));
    }
    if !normalized.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(KabiPayError::Validation(
            "component code may contain only letters, numbers, hyphen, and underscore".into(),
        ));
    }
    Ok(normalized)
}

fn normalize_component_type(component_type: &str) -> KabiPayResult<String> {
    let normalized = component_type.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "EARNING" | "DEDUCTION" | "EMPLOYER_CONTRIBUTION" => Ok(normalized),
        _ => Err(KabiPayError::Validation(
            "component type must be EARNING, DEDUCTION, or EMPLOYER_CONTRIBUTION".into(),
        )),
    }
}

fn normalize_calculation_basis(calculation_basis: &str) -> KabiPayResult<String> {
    let normalized = calculation_basis.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "FIXED_ANNUAL" | "FIXED_MONTHLY" | "PERCENT_OF_CTC" | "PERCENT_OF_BASIC" => Ok(normalized),
        _ => Err(KabiPayError::Validation(
            "calculation basis must be FIXED_ANNUAL, FIXED_MONTHLY, PERCENT_OF_CTC, or PERCENT_OF_BASIC".into(),
        )),
    }
}

pub fn parse_money_decimal(raw: &str, field: &'static str) -> KabiPayResult<Decimal> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(KabiPayError::Validation(format!("{field} must not be empty")));
    }
    let parsed = Decimal::from_str(trimmed)
        .map_err(|e| KabiPayError::Validation(format!("{field}: {e}")))?;
    if parsed < Decimal::ZERO {
        return Err(KabiPayError::Validation(format!("{field} must not be negative")));
    }
    Ok(parsed)
}

pub async fn upsert_salary_component(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    code: String,
    component_type: String,
    is_taxable: bool,
    is_fixed: bool,
    is_active: bool,
    formula_expression: Option<String>,
) -> KabiPayResult<salary_component::Model> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KabiPayError::Validation("component name must not be empty".into()));
    }
    let code = normalize_component_code(&code)?;
    let component_type = normalize_component_type(&component_type)?;
    let now = Utc::now();
    if let Some(id) = id {
        let existing = salary_component::Entity::find()
            .filter(salary_component::Column::Id.eq(id))
            .filter(salary_component::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "salary_component",
                id: id.to_string(),
            })?;
        let mut active: salary_component::ActiveModel = existing.into();
        active.name = Set(name.to_string());
        active.code = Set(code);
        active.r#type = Set(component_type);
        active.is_taxable = Set(is_taxable);
        active.is_fixed = Set(is_fixed);
        active.is_active = Set(is_active);
        active.formula_expression = Set(formula_expression.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        }));
        active.updated_at = Set(now);
        return active.update(db).await.map_err(KabiPayError::from);
    }
    salary_component::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        name: Set(name.to_string()),
        code: Set(code),
        r#type: Set(component_type),
        is_taxable: Set(is_taxable),
        is_fixed: Set(is_fixed),
        is_active: Set(is_active),
        formula_expression: Set(formula_expression.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)
}

/// Insert a **DRAFT** `payroll_cycle` for a calendar month. Rejects if a cycle for the same
/// tenant + month + year already exists.
pub async fn create_payroll_cycle(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    name: String,
    month: i32,
    year: i32,
    payment_date: Option<NaiveDate>,
) -> KabiPayResult<payroll_cycle::Model> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KabiPayError::Validation("name must not be empty".into()));
    }
    if !(1..=12).contains(&month) {
        return Err(KabiPayError::Validation("month must be 1–12".into()));
    }
    if !(2000..=2200).contains(&year) {
        return Err(KabiPayError::Validation("year must be between 2000 and 2200".into()));
    }

    let existing = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    if existing.is_some() {
        return Err(KabiPayError::Validation(format!(
            "a payroll cycle already exists for {month:02}/{year}"
        )));
    }

    let id = Uuid::new_v4();
    let now = Utc::now();
    let inserted = payroll_cycle::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        name: Set(name.to_string()),
        month: Set(month),
        year: Set(year),
        status: Set("DRAFT".to_string()),
        payment_date: Set(payment_date),
        processed_by: Set(None),
        processed_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)?;
    Ok(inserted)
}

pub async fn list_cycles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<payroll_cycle::Model>> {
    let limit = limit.clamp(1, 60);
    payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .order_by_desc(payroll_cycle::Column::Year)
        .order_by_desc(payroll_cycle::Column::Month)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_salary_structures(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<(salary_structure::Model, Vec<(salary_structure_component::Model, salary_component::Model)>)>> {
    let limit = limit.clamp(1, 100);
    let structures = salary_structure::Entity::find()
        .filter(salary_structure::Column::TenantId.eq(tenant_id))
        .order_by_asc(salary_structure::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let structure_ids = structures.iter().map(|s| s.id).collect::<Vec<_>>();
    if structure_ids.is_empty() {
        return Ok(vec![]);
    }
    let components = salary_structure_component::Entity::find()
        .filter(salary_structure_component::Column::TenantId.eq(tenant_id))
        .filter(salary_structure_component::Column::SalaryStructureId.is_in(structure_ids))
        .order_by_asc(salary_structure_component::Column::DisplayOrder)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let component_ids = components.iter().map(|c| c.salary_component_id).collect::<Vec<_>>();
    let component_rows = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant_id))
        .filter(salary_component::Column::Id.is_in(component_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let component_map = component_rows.into_iter().map(|c| (c.id, c)).collect::<HashMap<_, _>>();
    let mut grouped: HashMap<Uuid, Vec<(salary_structure_component::Model, salary_component::Model)>> = HashMap::new();
    for row in components {
        if let Some(component) = component_map.get(&row.salary_component_id) {
            grouped.entry(row.salary_structure_id).or_default().push((row, component.clone()));
        }
    }
    Ok(structures
        .into_iter()
        .map(|structure| {
            let lines = grouped.remove(&structure.id).unwrap_or_default();
            (structure, lines)
        })
        .collect())
}

pub async fn upsert_salary_structure(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    description: Option<String>,
    components: Vec<(Uuid, String, Decimal, i32)>,
) -> KabiPayResult<(salary_structure::Model, Vec<(salary_structure_component::Model, salary_component::Model)>)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KabiPayError::Validation("salary structure name must not be empty".into()));
    }
    if components.is_empty() {
        return Err(KabiPayError::Validation("salary structure must contain at least one component".into()));
    }
    let now = Utc::now();
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let structure = if let Some(id) = id {
        let existing = salary_structure::Entity::find()
            .filter(salary_structure::Column::Id.eq(id))
            .filter(salary_structure::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "salary_structure",
                id: id.to_string(),
            })?;
        let mut active: salary_structure::ActiveModel = existing.into();
        active.name = Set(name.to_string());
        active.description = Set(description.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        }));
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(KabiPayError::from)?
    } else {
        salary_structure::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            name: Set(name.to_string()),
            description: Set(description.and_then(|s| {
                let t = s.trim().to_string();
                if t.is_empty() { None } else { Some(t) }
            })),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(KabiPayError::from)?
    };

    salary_structure_component::Entity::delete_many()
        .filter(salary_structure_component::Column::TenantId.eq(tenant_id))
        .filter(salary_structure_component::Column::SalaryStructureId.eq(structure.id))
        .exec(&txn)
        .await
        .map_err(KabiPayError::from)?;

    for (component_id, basis, value, display_order) in components {
        let basis = normalize_calculation_basis(&basis)?;
        let component_exists = salary_component::Entity::find()
            .filter(salary_component::Column::Id.eq(component_id))
            .filter(salary_component::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?
            .is_some();
        if !component_exists {
            return Err(KabiPayError::NotFound {
                entity: "salary_component",
                id: component_id.to_string(),
            });
        }
        salary_structure_component::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            salary_structure_id: Set(structure.id),
            salary_component_id: Set(component_id),
            amount: Set(if basis == "FIXED_ANNUAL" { Some(value) } else { None }),
            percentage_of_basic: Set(if basis == "PERCENT_OF_BASIC" { Some(value) } else { None }),
            calculation_basis: Set(basis),
            calculation_value: Set(Some(value)),
            display_order: Set(display_order),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(KabiPayError::from)?;
    }

    txn.commit().await.map_err(KabiPayError::from)?;
    let rows = list_salary_structures(db, tenant_id, 100).await?;
    rows.into_iter()
        .find(|(row, _)| row.id == structure.id)
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "salary_structure",
            id: structure.id.to_string(),
        })
}

/// Payslips for a tenant, optionally restricted to one employee, newest first.
pub async fn list_payslips(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Option<Uuid>,
    limit: u64,
) -> KabiPayResult<Vec<payslip::Model>> {
    let limit = limit.clamp(1, 60);
    let mut q = payslip::Entity::find().filter(payslip::Column::TenantId.eq(tenant_id));
    if let Some(e) = employee_id {
        q = q.filter(payslip::Column::EmployeeId.eq(e));
    }
    q.order_by_desc(payslip::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// One payslip with its `payslip_component` rows.
pub async fn find_payslip_detail(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> KabiPayResult<Option<(payslip::Model, Vec<payslip_component::Model>)>> {
    let Some(m) = payslip::Entity::find()
        .filter(payslip::Column::Id.eq(id))
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let comps = payslip_component::Entity::find()
        .filter(payslip_component::Column::TenantId.eq(tenant_id))
        .filter(payslip_component::Column::PayslipId.eq(id))
        .order_by_asc(payslip_component::Column::CreatedAt)
        .all(db)
        .await?;
    Ok(Some((m, comps)))
}

/// Batch lines for all payslips returned by [`list_payslips`].
pub async fn payslip_lines_by_payslip_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    payslip_ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, Vec<payslip_component::Model>>> {
    if payslip_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let comps = payslip_component::Entity::find()
        .filter(payslip_component::Column::TenantId.eq(tenant_id))
        .filter(payslip_component::Column::PayslipId.is_in(payslip_ids.to_vec()))
        .all(db)
        .await?;
    let mut map: HashMap<Uuid, Vec<payslip_component::Model>> = HashMap::new();
    for c in comps {
        map.entry(c.payslip_id).or_default().push(c);
    }
    Ok(map)
}

fn csv_cell(raw: &str) -> String {
    if raw.contains(',') || raw.contains('"') || raw.contains('\n') || raw.contains('\r') {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_string()
    }
}

fn dec_cell(d: Decimal) -> String {
    csv_cell(&d.normalize().to_string())
}

/// Fallback when **`payroll_compliance_setting`** has no non-empty value: env
/// `KABIPAY_PAYROLL_EMPLOYER_TAN` / `KABIPAY_PAYROLL_EMPLOYER_LEGAL_NAME` on the payroll process.
fn payroll_export_employer_tan_env() -> String {
    std::env::var("KABIPAY_PAYROLL_EMPLOYER_TAN")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn payroll_export_employer_legal_name_env() -> String {
    std::env::var("KABIPAY_PAYROLL_EMPLOYER_LEGAL_NAME")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// **India statutory CSV exports:** prefer tenant **`payroll_compliance_setting`**, else env fallbacks.
pub async fn resolved_employer_placeholders_for_exports(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<(String, String)> {
    let row = payroll_compliance_setting::Entity::find()
        .filter(payroll_compliance_setting::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let tan = row
        .as_ref()
        .and_then(|r| r.employer_tan.as_deref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(payroll_export_employer_tan_env);
    let legal = row
        .as_ref()
        .and_then(|r| r.employer_legal_name.as_deref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(payroll_export_employer_legal_name_env);
    Ok((tan, legal))
}

fn trim_opt(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// One optional row per tenant — employer TAN and legal name shown on statutory payroll CSV exports.
pub async fn find_payroll_compliance_setting(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Option<payroll_compliance_setting::Model>> {
    payroll_compliance_setting::Entity::find()
        .filter(payroll_compliance_setting::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

fn norm_component_code(inp: Option<String>, fallback: &'static str) -> String {
    trim_opt(inp).unwrap_or_else(|| fallback.to_string())
}

/// Insert or update **`payroll_compliance_setting`** for the tenant (`tenant_id` from JWT scope).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_payroll_compliance_setting(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employer_tan: Option<String>,
    employer_legal_name: Option<String>,
    base_salary_component_code: Option<String>,
    arrear_salary_component_code: Option<String>,
    payslip_header_title: Option<String>,
    payslip_logo_file_storage_id: Option<Uuid>,
) -> KabiPayResult<payroll_compliance_setting::Model> {
    let tan_o = trim_opt(employer_tan);
    let legal_o = trim_opt(employer_legal_name);
    let base_code = norm_component_code(base_salary_component_code, "BASIC");
    let arrear_code = norm_component_code(arrear_salary_component_code, "ARREAR");
    let title_o = trim_opt(payslip_header_title);

    let now = Utc::now();
    if let Some(m) = find_payroll_compliance_setting(db, tenant_id).await? {
        let mut active: payroll_compliance_setting::ActiveModel = m.into();
        active.employer_tan = sea_orm::ActiveValue::Set(tan_o);
        active.employer_legal_name = sea_orm::ActiveValue::Set(legal_o);
        active.base_salary_component_code = Set(base_code.clone());
        active.arrear_salary_component_code = Set(arrear_code.clone());
        active.payslip_header_title = Set(title_o);
        active.payslip_logo_file_storage_id = Set(payslip_logo_file_storage_id);
        active.updated_at = sea_orm::ActiveValue::Set(now);
        active
            .update(db)
            .await
            .map_err(KabiPayError::from)
    } else {
        let id = Uuid::new_v4();
        payroll_compliance_setting::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            employer_tan: Set(tan_o),
            employer_legal_name: Set(legal_o),
            base_salary_component_code: Set(base_code),
            arrear_salary_component_code: Set(arrear_code),
            payslip_header_title: Set(title_o),
            payslip_logo_file_storage_id: Set(payslip_logo_file_storage_id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(KabiPayError::from)
    }
}

/// India payroll stub: one CSV listing all payslips in a payroll cycle (month + year) with TDS and PAN.
/// Header is always present; body is empty when no matching cycle or no payslips.
pub async fn india_tds_monthly_summary_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "employee_code,employee_name,pan,period_month,period_year,payroll_cycle_name,gross_salary,total_deductions,tds_amount,net_salary,payslip_status,payslip_id\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let pans = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_pan::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut pan_by_emp: HashMap<Uuid, String> = HashMap::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    for p in pans {
        if seen.insert(p.employee_id) {
            pan_by_emp.insert(p.employee_id, p.pan_number);
        }
    }

    let cycle_name = &cycle_row.name;
    for p in slips {
        let (code, name) = match emp_map.get(&p.employee_id) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let pan = pan_by_emp
            .get(&p.employee_id)
            .map(String::as_str)
            .unwrap_or("");
        let tds = p.tds_amount.unwrap_or(Decimal::ZERO);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_cell(code),
            csv_cell(&name),
            csv_cell(pan),
            month,
            year,
            csv_cell(cycle_name),
            dec_cell(p.gross_salary),
            dec_cell(p.total_deductions),
            dec_cell(tds),
            dec_cell(p.net_salary),
            csv_cell(&p.status),
            csv_cell(&p.id.to_string()),
        ));
    }

    Ok(out)
}

/// India payroll stub: PF + ESI columns from `payslip` for all rows in the payroll cycle (`month` + `year`).
/// Same RBAC as TDS export; not an ECR / challan file — statutory prep only.
pub async fn india_pf_esi_monthly_summary_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "employee_code,employee_name,pan,uan_number,esic_number,period_month,period_year,payroll_cycle_name,pf_employee,pf_employer,esi_employee,esi_employer,gross_salary,payslip_status,payslip_id\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let pans = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_pan::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut pan_by_emp: HashMap<Uuid, String> = HashMap::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    for p in pans {
        if seen.insert(p.employee_id) {
            pan_by_emp.insert(p.employee_id, p.pan_number);
        }
    }

    let cycle_name = &cycle_row.name;
    let z = Decimal::ZERO;
    for p in slips {
        let (code, name) = match emp_map.get(&p.employee_id) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let pan = pan_by_emp
            .get(&p.employee_id)
            .map(String::as_str)
            .unwrap_or("");
        let uan = p.uan_number.as_deref().unwrap_or("");
        let esic = p.esic_number.as_deref().unwrap_or("");
        let pf_e = p.pf_employee.unwrap_or(z);
        let pf_r = p.pf_employer.unwrap_or(z);
        let esi_e = p.esi_employee.unwrap_or(z);
        let esi_r = p.esi_employer.unwrap_or(z);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_cell(code),
            csv_cell(&name),
            csv_cell(pan),
            csv_cell(uan),
            csv_cell(esic),
            month,
            year,
            csv_cell(cycle_name),
            dec_cell(pf_e),
            dec_cell(pf_r),
            dec_cell(esi_e),
            dec_cell(esi_r),
            dec_cell(p.gross_salary),
            csv_cell(&p.status),
            csv_cell(&p.id.to_string()),
        ));
    }

    Ok(out)
}

/// **Bank disbursement (CSV).** One row per payslip in the payroll cycle for `month` + `year`, with
/// the employee’s **primary** `employee_bank` when present. `net_salary` is the transfer amount; not
/// a bank NEFT/RTGS file format from any one bank—generic prep for upload / ops.
pub async fn payroll_bank_transfer_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "employee_code,employee_name,beneficiary_name,bank_name,account_number,ifsc_code,account_type,currency,amount,period_month,period_year,payroll_cycle_name,bank_status,payslip_id\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let bank_rows = employee_bank::Entity::find()
        .filter(employee_bank::Column::TenantId.eq(tenant_id))
        .filter(employee_bank::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_bank::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut bank_by_emp: HashMap<Uuid, employee_bank::Model> = HashMap::new();
    for b in bank_rows {
        bank_by_emp.entry(b.employee_id).or_insert(b);
    }

    let cycle_name = &cycle_row.name;
    for p in slips {
        let (code, name) = match emp_map.get(&p.employee_id) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let (bank_status, bname, acc, ifsc, atype) = if let Some(b) = bank_by_emp.get(&p.employee_id) {
            (
                "OK",
                b.bank_name.as_str(),
                b.account_number.as_str(),
                b.ifsc_code.as_str(),
                b.account_type.as_deref().unwrap_or(""),
            )
        } else {
            ("MISSING_BANK", "", "", "", "")
        };
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_cell(code),
            csv_cell(&name),
            csv_cell(&name),
            csv_cell(bname),
            csv_cell(acc),
            csv_cell(ifsc),
            csv_cell(atype),
            csv_cell("INR"),
            dec_cell(p.net_salary),
            month,
            year,
            csv_cell(cycle_name),
            bank_status,
            csv_cell(&p.id.to_string()),
        ));
    }

    Ok(out)
}

fn month_abbr_en(month: i32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "?",
    }
}

/// **India — NEFT / bulk salary credit prep (CSV).** Same payslip rows and primary `employee_bank` as
/// `payroll_bank_transfer_csv`, with columns oriented toward common corporate **multi-beneficiary NEFT**
/// spreadsheets (beneficiary IFSC/account, narration, optional value date). Not NPCI ACH **NACH** mandate
/// format or any one bank’s binary upload — operational prep only.
pub async fn payroll_india_bulk_neft_credit_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "serial_no,beneficiary_name,beneficiary_account_number,ifsc_code,amount_inr,value_date_iso,txn_type,narration,employee_code,payroll_cycle_month,payroll_cycle_year,cycle_name,bank_status,payslip_id\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let bank_rows = employee_bank::Entity::find()
        .filter(employee_bank::Column::TenantId.eq(tenant_id))
        .filter(employee_bank::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_bank::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut bank_by_emp: HashMap<Uuid, employee_bank::Model> = HashMap::new();
    for b in bank_rows {
        bank_by_emp.entry(b.employee_id).or_insert(b);
    }

    let cycle_name = &cycle_row.name;
    let value_date = cycle_row
        .payment_date
        .map(|d| d.to_string())
        .unwrap_or_default();

    let mut seq: i32 = 0;
    for p in slips {
        seq += 1;
        let (code, disp_name) = match emp_map.get(&p.employee_id) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let (bank_status, acc, ifsc) = if let Some(b) = bank_by_emp.get(&p.employee_id) {
            ("OK", b.account_number.as_str(), b.ifsc_code.as_str())
        } else {
            ("MISSING_BANK", "", "")
        };
        let narration = format!("SALARY {} {} {}", month_abbr_en(month), year, code);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            seq,
            csv_cell(&disp_name),
            csv_cell(acc),
            csv_cell(ifsc),
            dec_cell(p.net_salary),
            csv_cell(&value_date),
            csv_cell("NEFT"),
            csv_cell(&narration),
            csv_cell(code),
            month,
            year,
            csv_cell(cycle_name),
            bank_status,
            csv_cell(&p.id.to_string()),
        ));
    }

    Ok(out)
}

/// EPF “wage” for reconciliation: `min(gross, ₹15,000)` — matches pay-run statutory **stub** ceiling.
fn epf_wage_stub_from_gross(gross_salary: Decimal) -> Decimal {
    use std::str::FromStr;
    let ceiling = Decimal::from_str(statutory_india::PF_WAGE_CEILING_INR).expect("const decimal");
    gross_salary.min(ceiling)
}

/// **India — Form 24Q salary payment month stub (CSV).** One row per payslip with PAN, India FY of the
/// pay month, calendar period, optional payment date from the cycle, gross as a **notional** Section 192
/// payment base, and `tds_amount`. **Not** TRACES-upload **Form 24Q**, **Annex II**, or validated file layout
/// — reconciliations & TAN/payment metadata are out of band.
pub async fn india_form24q_salary_payment_monthly_stub_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "export_kind,period_month,period_year,india_fy_start_year,payment_date_iso,employee_code,employee_name,person_pan,amount_paid_credited_salary_section192_stub,income_tax_deducted_section192,payslip_status,payslip_id,employer_tan_env,employer_name_env\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let fy = statutory_india::india_fy_start_year(month, year);
    let pmnt = cycle_row.payment_date.map(|d| d.to_string()).unwrap_or_default();

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let pans = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_pan::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut pan_by_emp: HashMap<Uuid, String> = HashMap::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    for p in pans {
        if seen.insert(p.employee_id) {
            pan_by_emp.insert(p.employee_id, p.pan_number);
        }
    }

    let z = Decimal::ZERO;
    let (employer_tan, employer_legal_name) =
        resolved_employer_placeholders_for_exports(db, tenant_id).await?;
    let employer_tan_csv = csv_cell(&employer_tan);
    let employer_name_csv = csv_cell(&employer_legal_name);
    for p in slips {
        let (code, name) = match emp_map.get(&p.employee_id) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let pan = pan_by_emp
            .get(&p.employee_id)
            .map(String::as_str)
            .unwrap_or("");
        let tds = p.tds_amount.unwrap_or(z);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_cell("FORM24Q_SALARY_MONTH_STUB"),
            month,
            year,
            fy,
            csv_cell(&pmnt),
            csv_cell(code),
            csv_cell(&name),
            csv_cell(pan),
            dec_cell(p.gross_salary),
            dec_cell(tds),
            csv_cell(&p.status),
            csv_cell(&p.id.to_string()),
            employer_tan_csv,
            employer_name_csv,
        ));
    }

    Ok(out)
}

/// **India — EPFO ECR-style monthly contribution prep (CSV).** Columns: UAN, member name, PAY month,
/// PAY year, capped **EPF wage** (`min(gross, ₹15,000)` per run stub), employee + employer EPF from payslip,
/// gross salary. Not the official **Unified EPF**/`ECR` binary or mandated column order — **only** reconciliation prep.
pub async fn india_epf_monthly_ecr_prep_stub_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    month: i32,
    year: i32,
) -> KabiPayResult<String> {
    if !(1..=12).contains(&month) || !(1900..=2200).contains(&year) {
        return Err(KabiPayError::Validation(
            "month must be 1–12 and year a plausible calendar year".into(),
        ));
    }

    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(payroll_cycle::Column::Month.eq(month))
        .filter(payroll_cycle::Column::Year.eq(year))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    let mut out = String::from(
        "export_kind,uan_number,member_name,pay_month,pay_year,epf_wage_stub_min_gross_or_ceiling,pf_employee,pf_employer,gross_salary,payslip_status,payslip_id\n",
    );

    let Some(cycle_row) = cycle else {
        return Ok(out);
    };

    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_row.id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let emp_ids: Vec<Uuid> = slips.iter().map(|p| p.employee_id).collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let z = Decimal::ZERO;
    for p in slips {
        let name = match emp_map.get(&p.employee_id) {
            Some(e) => format!("{} {}", e.first_name, e.last_name),
            None => String::new(),
        };
        let uan = p.uan_number.as_deref().unwrap_or("");
        let wage = epf_wage_stub_from_gross(p.gross_salary);
        let pf_e = p.pf_employee.unwrap_or(z);
        let pf_r = p.pf_employer.unwrap_or(z);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_cell("EPF_ECR_PREP_STUB"),
            csv_cell(uan),
            csv_cell(&name),
            month,
            year,
            dec_cell(wage),
            dec_cell(pf_e),
            dec_cell(pf_r),
            dec_cell(p.gross_salary),
            csv_cell(&p.status),
            csv_cell(&p.id.to_string()),
        ));
    }

    Ok(out)
}

fn india_fy_full_year_cycle_condition(fy_start_year: i32) -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(payroll_cycle::Column::Year.eq(fy_start_year))
                .add(payroll_cycle::Column::Month.between(4, 12)),
        )
        .add(
            Condition::all()
                .add(payroll_cycle::Column::Year.eq(fy_start_year + 1))
                .add(payroll_cycle::Column::Month.between(1, 3)),
        )
}

/// India FY calendar quarter within April–March FY: Q1 Apr–Jun, Q2 Jul–Sep, Q3 Oct–Dec (all `fy_start_year`),
/// Q4 Jan–Mar (`fy_start_year + 1`).
fn india_fy_quarter_cycle_condition(
    fy_start_year: i32,
    quarter: i32,
) -> KabiPayResult<Condition> {
    if !(1..=4).contains(&quarter) {
        return Err(KabiPayError::Validation(
            "quarter must be 1–4 (India FY: Q1 Apr–Jun … Q4 Jan–Mar of next calendar year)".into(),
        ));
    }
    Ok(match quarter {
        1 => Condition::all()
            .add(payroll_cycle::Column::Year.eq(fy_start_year))
            .add(payroll_cycle::Column::Month.between(4, 6)),
        2 => Condition::all()
            .add(payroll_cycle::Column::Year.eq(fy_start_year))
            .add(payroll_cycle::Column::Month.between(7, 9)),
        3 => Condition::all()
            .add(payroll_cycle::Column::Year.eq(fy_start_year))
            .add(payroll_cycle::Column::Month.between(10, 12)),
        4 => Condition::all()
            .add(payroll_cycle::Column::Year.eq(fy_start_year + 1))
            .add(payroll_cycle::Column::Month.between(1, 3)),
        _ => unreachable!("quarter validated above"),
    })
}

fn india_fy_quarter_label(quarter: i32) -> &'static str {
    match quarter {
        1 => "Q1_Apr_Jun",
        2 => "Q2_Jul_Sep",
        3 => "Q3_Oct_Dec",
        4 => "Q4_Jan_Mar",
        _ => "Q?",
    }
}

#[derive(Clone, Copy)]
enum IndiaFyEmployeeAggCsvKind {
    /// Full India FY (Apr–Mar).
    FyTotals,
    /// One FY quarter only (for 24Q-style quarterly reconciliation).
    Quarter { quarter: i32 },
    /// Form 16 Part B–oriented column names; not a Part B PDF or legal certificate.
    Form16PartBStub,
}

async fn india_fy_period_employee_aggregates_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    fy_start_year: i32,
    kind: IndiaFyEmployeeAggCsvKind,
) -> KabiPayResult<String> {
    if !(2000..=2199).contains(&fy_start_year) {
        return Err(KabiPayError::Validation(
            "fyStartYear must be a plausible India FY start year (e.g. 2025 for FY 2025–26)".into(),
        ));
    }

    let period_clause = match kind {
        IndiaFyEmployeeAggCsvKind::FyTotals | IndiaFyEmployeeAggCsvKind::Form16PartBStub => {
            india_fy_full_year_cycle_condition(fy_start_year)
        }
        IndiaFyEmployeeAggCsvKind::Quarter { quarter } => {
            india_fy_quarter_cycle_condition(fy_start_year, quarter)?
        }
    };

    let cycles = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .filter(period_clause)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let header = match kind {
        IndiaFyEmployeeAggCsvKind::FyTotals => {
            "india_fy_start_year,india_fy_label,employee_code,employee_name,pan,payslip_count,sum_gross_salary,sum_total_deductions,sum_net_salary,sum_tds_amount,sum_pf_employee,sum_esi_employee,sum_professional_tax\n"
        }
        IndiaFyEmployeeAggCsvKind::Quarter { .. } => {
            "export_kind,india_fy_start_year,india_fy_label,quarter,quarter_label,employee_code,employee_name,pan,payslip_count,sum_gross_salary,sum_total_deductions,sum_net_salary,sum_tds_amount,sum_pf_employee,sum_esi_employee,sum_professional_tax\n"
        }
        IndiaFyEmployeeAggCsvKind::Form16PartBStub => {
            "export_notice,india_fy_start_year,india_fy_label,employer_tan_placeholder,employer_name_placeholder,employee_code,employee_name,employee_pan,payslip_rows_in_fy,partb_gross_salary_prep,partb_total_deductions_prep,partb_net_amount_prep,partb_sum_tds_on_salary_prep,partb_sum_pf_employee_prep,partb_sum_esi_employee_prep,partb_sum_professional_tax_prep\n"
        }
    };
    let mut out = String::from(header);

    if cycles.is_empty() {
        return Ok(out);
    }

    let cycle_ids: Vec<Uuid> = cycles.iter().map(|c| c.id).collect();
    let slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.is_in(cycle_ids))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if slips.is_empty() {
        return Ok(out);
    }

    let z = Decimal::ZERO;
    #[derive(Default, Clone)]
    struct Agg {
        gross: Decimal,
        deductions: Decimal,
        net: Decimal,
        tds: Decimal,
        pf_e: Decimal,
        esi_e: Decimal,
        pt: Decimal,
        count: usize,
    }
    let mut by_emp: HashMap<Uuid, Agg> = HashMap::new();
    for p in slips {
        let e = by_emp.entry(p.employee_id).or_default();
        e.gross += p.gross_salary;
        e.deductions += p.total_deductions;
        e.net += p.net_salary;
        e.tds += p.tds_amount.unwrap_or(z);
        e.pf_e += p.pf_employee.unwrap_or(z);
        e.esi_e += p.esi_employee.unwrap_or(z);
        e.pt += p.professional_tax.unwrap_or(z);
        e.count += 1;
    }

    let fy_label = format!("FY{}-{}", fy_start_year, fy_start_year + 1);
    let emp_ids: Vec<Uuid> = by_emp.keys().copied().collect();
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Id.is_in(emp_ids.clone()))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let emp_map: HashMap<Uuid, employee::Model> =
        employees.into_iter().map(|e| (e.id, e)).collect();

    let pans = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.is_in(emp_ids))
        .filter(employee_pan::Column::IsPrimary.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut pan_by_emp: HashMap<Uuid, String> = HashMap::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    for p in pans {
        if seen.insert(p.employee_id) {
            pan_by_emp.insert(p.employee_id, p.pan_number);
        }
    }

    let mut keys: Vec<Uuid> = by_emp.keys().copied().collect();
    keys.sort_by_key(|id| {
        emp_map
            .get(id)
            .map(|e| e.employee_code.clone())
            .unwrap_or_default()
    });

    let form16_employer_cells: Option<(String, String)> =
        if matches!(kind, IndiaFyEmployeeAggCsvKind::Form16PartBStub) {
            let (t, l) =
                resolved_employer_placeholders_for_exports(db, tenant_id).await?;
            Some((csv_cell(&t), csv_cell(&l)))
        } else {
            None
        };

    for eid in keys {
        let agg = by_emp.get(&eid).cloned().unwrap_or_default();
        let (code, name) = match emp_map.get(&eid) {
            Some(e) => (
                e.employee_code.as_str(),
                format!("{} {}", e.first_name, e.last_name),
            ),
            None => ("", String::new()),
        };
        let pan = pan_by_emp
            .get(&eid)
            .map(String::as_str)
            .unwrap_or("");
        match kind {
            IndiaFyEmployeeAggCsvKind::FyTotals => {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    fy_start_year,
                    csv_cell(&fy_label),
                    csv_cell(code),
                    csv_cell(&name),
                    csv_cell(pan),
                    agg.count,
                    dec_cell(agg.gross),
                    dec_cell(agg.deductions),
                    dec_cell(agg.net),
                    dec_cell(agg.tds),
                    dec_cell(agg.pf_e),
                    dec_cell(agg.esi_e),
                    dec_cell(agg.pt),
                ));
            }
            IndiaFyEmployeeAggCsvKind::Quarter { quarter } => {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    csv_cell("INDIA_FY_QUARTER_TOTALS"),
                    fy_start_year,
                    csv_cell(&fy_label),
                    quarter,
                    csv_cell(india_fy_quarter_label(quarter)),
                    csv_cell(code),
                    csv_cell(&name),
                    csv_cell(pan),
                    agg.count,
                    dec_cell(agg.gross),
                    dec_cell(agg.deductions),
                    dec_cell(agg.net),
                    dec_cell(agg.tds),
                    dec_cell(agg.pf_e),
                    dec_cell(agg.esi_e),
                    dec_cell(agg.pt),
                ));
            }
            IndiaFyEmployeeAggCsvKind::Form16PartBStub => {
                let (employer_tan_csv, employer_legal_name_csv) = form16_employer_cells
                    .as_ref()
                    .expect("Form16 stub branch always loads employer placeholder cells");
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    csv_cell("FORM16_PARTB_FY_PREP_STUB"),
                    fy_start_year,
                    csv_cell(&fy_label),
                    employer_tan_csv,
                    employer_legal_name_csv,
                    csv_cell(code),
                    csv_cell(&name),
                    csv_cell(pan),
                    agg.count,
                    dec_cell(agg.gross),
                    dec_cell(agg.deductions),
                    dec_cell(agg.net),
                    dec_cell(agg.tds),
                    dec_cell(agg.pf_e),
                    dec_cell(agg.esi_e),
                    dec_cell(agg.pt),
                ));
            }
        }
    }

    Ok(out)
}

/// **India FY — employee totals across payslips (CSV).** Sums gross, deductions, net, TDS, PF employee,
/// ESI employee, and PT for every payslip belonging to payroll cycles whose **India financial year**
/// matches `fy_start_year` (April `fy_start_year` through March `fy_start_year + 1`). Stub for **Form 16 /
/// annual compliance prep** — not a Part B PDF or filed return.
pub async fn india_fy_payroll_employee_totals_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    fy_start_year: i32,
) -> KabiPayResult<String> {
    india_fy_period_employee_aggregates_csv(
        db,
        tenant_id,
        fy_start_year,
        IndiaFyEmployeeAggCsvKind::FyTotals,
    )
    .await
}

/// **India FY quarter — employee totals (CSV).** Same measures as **`india_fy_payroll_employee_totals_csv`**, but only
/// cycles in **Q1** (Apr–Jun) … **Q4** (Jan–Mar next calendar year). For **Form 24Q** quarterly reconciliation style
/// prep — not filed return layout.
pub async fn india_fy_quarter_payroll_employee_totals_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    fy_start_year: i32,
    quarter: i32,
) -> KabiPayResult<String> {
    india_fy_period_employee_aggregates_csv(
        db,
        tenant_id,
        fy_start_year,
        IndiaFyEmployeeAggCsvKind::Quarter { quarter },
    )
    .await
}

/// **India FY — Form 16 Part B prep (stub CSV).** Same underlying aggregates as the FY totals export with
/// Part B–oriented column names and blank **`employer_tan_placeholder`** / **`employer_name_placeholder`** for
/// offline merge. Not a certificate or PDF.
pub async fn india_form16_part_b_fy_prep_stub_csv(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    fy_start_year: i32,
) -> KabiPayResult<String> {
    india_fy_period_employee_aggregates_csv(
        db,
        tenant_id,
        fy_start_year,
        IndiaFyEmployeeAggCsvKind::Form16PartBStub,
    )
    .await
}

/// Latest `employment_history.salary` for payroll gross (v1 pay run).
async fn latest_employment_salary<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Option<Decimal>> {
    let row = employment_history::Entity::find()
        .filter(employment_history::Column::TenantId.eq(tenant_id))
        .filter(employment_history::Column::EmployeeId.eq(employee_id))
        .filter(employment_history::Column::IsDeleted.eq(false))
        .order_by_desc(employment_history::Column::EffectiveFrom)
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(row.and_then(|r| r.salary))
}

#[derive(Clone, Debug)]
pub struct SalaryBreakupLine {
    pub salary_component_id: Uuid,
    pub component_name: String,
    pub component_code: String,
    pub component_type: String,
    pub calculation_basis: String,
    pub calculation_value: Decimal,
    pub annual_amount: Decimal,
    pub monthly_amount: Decimal,
    pub is_override: bool,
}

#[derive(Clone, Debug)]
pub struct SalaryBreakup {
    pub employee_id: Uuid,
    pub employee_salary_structure_id: Option<Uuid>,
    pub annual_ctc: Decimal,
    pub monthly_gross: Decimal,
    pub monthly_deductions: Decimal,
    pub monthly_net_before_statutory: Decimal,
    pub lines: Vec<SalaryBreakupLine>,
}

fn period_start(month: i32, year: i32) -> KabiPayResult<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month as u32, 1).ok_or_else(|| {
        KabiPayError::Validation(format!("invalid payroll period {month:02}/{year}"))
    })
}

async fn active_employee_salary_structure<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    as_of: NaiveDate,
) -> KabiPayResult<Option<employee_salary_structure::Model>> {
    employee_salary_structure::Entity::find()
        .filter(employee_salary_structure::Column::TenantId.eq(tenant_id))
        .filter(employee_salary_structure::Column::EmployeeId.eq(employee_id))
        .filter(employee_salary_structure::Column::EffectiveFrom.lte(as_of))
        .filter(
            Condition::any()
                .add(employee_salary_structure::Column::EffectiveTo.is_null())
                .add(employee_salary_structure::Column::EffectiveTo.gte(as_of)),
        )
        .order_by_desc(employee_salary_structure::Column::EffectiveFrom)
        .order_by_desc(employee_salary_structure::Column::UpdatedAt)
        .order_by_desc(employee_salary_structure::Column::CreatedAt)
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

fn amount_from_rule(
    basis: &str,
    value: Decimal,
    annual_ctc: Decimal,
    annual_basic: Decimal,
) -> KabiPayResult<Decimal> {
    match basis {
        "FIXED_ANNUAL" => Ok(value),
        "FIXED_MONTHLY" => Ok(value * Decimal::from(12)),
        "PERCENT_OF_CTC" => Ok((annual_ctc * value / Decimal::from(100)).round_dp(2)),
        "PERCENT_OF_BASIC" => Ok((annual_basic * value / Decimal::from(100)).round_dp(2)),
        _ => Err(KabiPayError::Validation(format!("unsupported calculation basis `{basis}`"))),
    }
}

fn component_rule_value(row: &salary_structure_component::Model) -> Decimal {
    row.calculation_value
        .or(row.percentage_of_basic)
        .or(row.amount)
        .unwrap_or(Decimal::ZERO)
}

fn resolve_basic_annual(
    rows: &[(salary_structure_component::Model, salary_component::Model)],
    annual_ctc: Decimal,
    base_code: &str,
    fallback_annual: Decimal,
) -> KabiPayResult<Decimal> {
    let base_code = base_code.trim().to_ascii_uppercase();
    let candidate = rows.iter().find(|(_, component)| {
        let code = component.code.trim().to_ascii_uppercase();
        code == base_code || code == "BASIC"
    });
    if let Some((row, _)) = candidate {
        let basis = normalize_calculation_basis(&row.calculation_basis)?;
        if basis == "PERCENT_OF_BASIC" {
            return Err(KabiPayError::Validation(
                "basic salary component cannot be calculated as percentage of basic".into(),
            ));
        }
        return amount_from_rule(
            &basis,
            component_rule_value(row),
            annual_ctc,
            Decimal::ZERO,
        );
    }
    if fallback_annual > Decimal::ZERO {
        return Ok(fallback_annual);
    }
    Ok(annual_ctc)
}

async fn salary_breakup_for_structure<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    employee_structure: employee_salary_structure::Model,
    base_code: &str,
    fallback_monthly_salary: Decimal,
) -> KabiPayResult<SalaryBreakup> {
    let structure_components = salary_structure_component::Entity::find()
        .filter(salary_structure_component::Column::TenantId.eq(tenant_id))
        .filter(salary_structure_component::Column::SalaryStructureId.eq(employee_structure.salary_structure_id))
        .order_by_asc(salary_structure_component::Column::DisplayOrder)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    if structure_components.is_empty() {
        return Err(KabiPayError::Validation(
            "assigned salary structure has no components".into(),
        ));
    }
    let component_ids = structure_components.iter().map(|c| c.salary_component_id).collect::<Vec<_>>();
    let components = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant_id))
        .filter(salary_component::Column::Id.is_in(component_ids))
        .filter(salary_component::Column::IsActive.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let component_map = components.into_iter().map(|c| (c.id, c)).collect::<HashMap<_, _>>();
    let rows = structure_components
        .into_iter()
        .filter_map(|row| component_map.get(&row.salary_component_id).cloned().map(|component| (row, component)))
        .collect::<Vec<_>>();
    let fallback_annual = (fallback_monthly_salary * Decimal::from(12)).round_dp(2);
    let annual_basic = resolve_basic_annual(&rows, employee_structure.ctc, base_code, fallback_annual)?;

    let overrides = employee_salary_component_override::Entity::find()
        .filter(employee_salary_component_override::Column::TenantId.eq(tenant_id))
        .filter(employee_salary_component_override::Column::EmployeeSalaryStructureId.eq(employee_structure.id))
        .filter(employee_salary_component_override::Column::IsActive.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let override_map = overrides.into_iter().map(|o| (o.salary_component_id, o)).collect::<HashMap<_, _>>();

    let mut lines = Vec::new();
    let mut seen_components = HashSet::new();
    for (row, component) in rows {
        let override_row = override_map.get(&component.id);
        let basis = override_row
            .map(|o| o.calculation_basis.clone())
            .unwrap_or_else(|| row.calculation_basis.clone());
        let basis = normalize_calculation_basis(&basis)?;
        let value = override_row
            .map(|o| o.calculation_value)
            .unwrap_or_else(|| component_rule_value(&row));
        let annual = amount_from_rule(&basis, value, employee_structure.ctc, annual_basic)?.round_dp(2);
        let monthly = (annual / Decimal::from(12)).round_dp(2);
        seen_components.insert(component.id);
        lines.push(SalaryBreakupLine {
            salary_component_id: component.id,
            component_name: component.name,
            component_code: component.code,
            component_type: component.r#type,
            calculation_basis: basis,
            calculation_value: value,
            annual_amount: annual,
            monthly_amount: monthly,
            is_override: override_row.is_some(),
        });
    }
    for (component_id, override_row) in override_map {
        if seen_components.contains(&component_id) {
            continue;
        }
        let Some(component) = salary_component::Entity::find()
            .filter(salary_component::Column::TenantId.eq(tenant_id))
            .filter(salary_component::Column::Id.eq(component_id))
            .filter(salary_component::Column::IsActive.eq(true))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
        else {
            continue;
        };
        let basis = normalize_calculation_basis(&override_row.calculation_basis)?;
        let annual = amount_from_rule(
            &basis,
            override_row.calculation_value,
            employee_structure.ctc,
            annual_basic,
        )?
        .round_dp(2);
        lines.push(SalaryBreakupLine {
            salary_component_id: component.id,
            component_name: component.name,
            component_code: component.code,
            component_type: component.r#type,
            calculation_basis: basis,
            calculation_value: override_row.calculation_value,
            annual_amount: annual,
            monthly_amount: (annual / Decimal::from(12)).round_dp(2),
            is_override: true,
        });
    }
    lines.sort_by(|a, b| a.component_code.cmp(&b.component_code));
    let monthly_gross: Decimal = lines
        .iter()
        .filter(|line| line.component_type.eq_ignore_ascii_case("EARNING"))
        .map(|line| line.monthly_amount)
        .sum();
    let monthly_deductions: Decimal = lines
        .iter()
        .filter(|line| line.component_type.eq_ignore_ascii_case("DEDUCTION"))
        .map(|line| line.monthly_amount)
        .sum();
    Ok(SalaryBreakup {
        employee_id,
        employee_salary_structure_id: Some(employee_structure.id),
        annual_ctc: employee_structure.ctc,
        monthly_gross: monthly_gross.round_dp(2),
        monthly_deductions: monthly_deductions.round_dp(2),
        monthly_net_before_statutory: (monthly_gross - monthly_deductions).round_dp(2),
        lines,
    })
}

pub async fn preview_employee_salary_breakup(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    as_of: NaiveDate,
) -> KabiPayResult<Option<SalaryBreakup>> {
    let comp_cfg = find_payroll_compliance_setting(db, tenant_id).await?;
    let base_code = comp_cfg
        .as_ref()
        .map(|c| c.base_salary_component_code.as_str())
        .unwrap_or("BASIC");
    let fallback = latest_employment_salary(db, tenant_id, employee_id)
        .await?
        .unwrap_or(Decimal::ZERO);
    let Some(structure) = active_employee_salary_structure(db, tenant_id, employee_id, as_of).await? else {
        return Ok(None);
    };
    salary_breakup_for_structure(db, tenant_id, employee_id, structure, base_code, fallback)
        .await
        .map(Some)
}

pub async fn assign_employee_salary_structure(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    salary_structure_id: Uuid,
    annual_ctc: Decimal,
    effective_from: NaiveDate,
    effective_to: Option<NaiveDate>,
    overrides: Vec<(Uuid, String, Decimal, Option<String>, bool)>,
) -> KabiPayResult<employee_salary_structure::Model> {
    if annual_ctc <= Decimal::ZERO {
        return Err(KabiPayError::Validation("annual CTC must be greater than zero".into()));
    }
    if let Some(to) = effective_to {
        if to < effective_from {
            return Err(KabiPayError::Validation("effectiveTo cannot be before effectiveFrom".into()));
        }
    }
    let now = Utc::now();
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let employee_exists = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.eq(employee_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .is_some();
    if !employee_exists {
        return Err(KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        });
    }
    let structure_exists = salary_structure::Entity::find()
        .filter(salary_structure::Column::TenantId.eq(tenant_id))
        .filter(salary_structure::Column::Id.eq(salary_structure_id))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .is_some();
    if !structure_exists {
        return Err(KabiPayError::NotFound {
            entity: "salary_structure",
            id: salary_structure_id.to_string(),
        });
    }
    let same_effective_row = employee_salary_structure::Entity::find()
        .filter(employee_salary_structure::Column::TenantId.eq(tenant_id))
        .filter(employee_salary_structure::Column::EmployeeId.eq(employee_id))
        .filter(employee_salary_structure::Column::EffectiveFrom.eq(effective_from))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if let Some(existing) = same_effective_row {
        let mut active: employee_salary_structure::ActiveModel = existing.into();
        active.salary_structure_id = Set(salary_structure_id);
        active.ctc = Set(annual_ctc);
        active.effective_to = Set(effective_to);
        active.updated_at = Set(now);
        let row = active.update(&txn).await.map_err(KabiPayError::from)?;
        replace_employee_salary_component_overrides(
            &txn,
            tenant_id,
            row.id,
            overrides,
            now,
        )
        .await?;
        txn.commit().await.map_err(KabiPayError::from)?;
        return Ok(row);
    }

    let previous_effective_to = effective_from.pred_opt().ok_or_else(|| {
        KabiPayError::Validation("effectiveFrom is too early to close previous salary assignment".into())
    })?;
    let overlapping_existing_rows = employee_salary_structure::Entity::find()
        .filter(employee_salary_structure::Column::TenantId.eq(tenant_id))
        .filter(employee_salary_structure::Column::EmployeeId.eq(employee_id))
        .filter(employee_salary_structure::Column::EffectiveFrom.lt(effective_from))
        .filter(
            Condition::any()
                .add(employee_salary_structure::Column::EffectiveTo.is_null())
                .add(employee_salary_structure::Column::EffectiveTo.gte(effective_from)),
        )
        .all(&txn)
        .await
        .map_err(KabiPayError::from)?;
    for existing in overlapping_existing_rows {
        let mut active: employee_salary_structure::ActiveModel = existing.into();
        active.effective_to = Set(Some(previous_effective_to));
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(KabiPayError::from)?;
    }

    let has_future_overlap = employee_salary_structure::Entity::find()
        .filter(employee_salary_structure::Column::TenantId.eq(tenant_id))
        .filter(employee_salary_structure::Column::EmployeeId.eq(employee_id))
        .filter(employee_salary_structure::Column::EffectiveFrom.gt(effective_from))
        .all(&txn)
        .await
        .map_err(KabiPayError::from)?
        .into_iter()
        .any(|existing| effective_to.map_or(true, |to| existing.effective_from <= to));
    if has_future_overlap {
        return Err(KabiPayError::Validation(
            "effective period overlaps an existing future salary assignment".into(),
        ));
    }

    let row = employee_salary_structure::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        salary_structure_id: Set(salary_structure_id),
        ctc: Set(annual_ctc),
        effective_from: Set(effective_from),
        effective_to: Set(effective_to),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;
    replace_employee_salary_component_overrides(&txn, tenant_id, row.id, overrides, now).await?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(row)
}

async fn replace_employee_salary_component_overrides<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    employee_salary_structure_id: Uuid,
    overrides: Vec<(Uuid, String, Decimal, Option<String>, bool)>,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    employee_salary_component_override::Entity::delete_many()
        .filter(employee_salary_component_override::Column::TenantId.eq(tenant_id))
        .filter(
            employee_salary_component_override::Column::EmployeeSalaryStructureId
                .eq(employee_salary_structure_id),
        )
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    for (component_id, basis, value, notes, is_active) in overrides {
        let basis = normalize_calculation_basis(&basis)?;
        employee_salary_component_override::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_salary_structure_id: Set(employee_salary_structure_id),
            salary_component_id: Set(component_id),
            calculation_basis: Set(basis),
            calculation_value: Set(value),
            notes: Set(notes.and_then(|s| {
                let t = s.trim().to_string();
                if t.is_empty() { None } else { Some(t) }
            })),
            is_active: Set(is_active),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(KabiPayError::from)?;
    }
    Ok(())
}

async fn find_active_salary_component_by_code<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    code: &str,
) -> KabiPayResult<Option<salary_component::Model>> {
    salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant_id))
        .filter(salary_component::Column::Code.eq(code))
        .filter(salary_component::Column::IsActive.eq(true))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

/// Resolve configured **primary earning** component for gross (tenant setting → `BASIC` → first EARNING).
async fn resolve_default_earning_component(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    configured_base_code: &str,
) -> KabiPayResult<salary_component::Model> {
    let try_codes = if configured_base_code.eq_ignore_ascii_case("BASIC") {
        vec![configured_base_code, "BASIC"]
    } else {
        vec![configured_base_code, "BASIC"]
    };
    let mut seen: HashSet<String> = HashSet::new();
    for code in try_codes {
        if !seen.insert(code.to_string()) {
            continue;
        }
        if let Some(c) = find_active_salary_component_by_code(db, tenant_id, code).await? {
            if !c.r#type.eq_ignore_ascii_case("EARNING") {
                return Err(KabiPayError::Validation(format!(
                    "salary component `{code}` must have type EARNING for the base payroll line",
                )));
            }
            return Ok(c);
        }
    }
    let rows = list_components(db, tenant_id, true, 50).await?;
    rows
        .into_iter()
        .find(|c| c.r#type.eq_ignore_ascii_case("EARNING"))
        .ok_or_else(|| {
            KabiPayError::Validation(
                "no active EARNING salary component — configure components (or set baseSalaryComponentCode) first"
                    .into(),
            )
        })
}

/// Active `EARNING` `salary_component` for arrear payouts (configured code, default **`ARREAR`**).
async fn resolve_arrear_salary_component<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    configured_arrear_code: &str,
) -> KabiPayResult<salary_component::Model> {
    let try_codes = if configured_arrear_code.eq_ignore_ascii_case("ARREAR") {
        vec![configured_arrear_code, "ARREAR"]
    } else {
        vec![configured_arrear_code, "ARREAR"]
    };
    let mut seen: HashSet<String> = HashSet::new();
    for code in try_codes {
        if !seen.insert(code.to_string()) {
            continue;
        }
        if let Some(c) = find_active_salary_component_by_code(db, tenant_id, code).await? {
            if !c.r#type.eq_ignore_ascii_case("EARNING") {
                return Err(KabiPayError::Validation(format!(
                    "salary component `{code}` must have type EARNING for arrear payout lines",
                )));
            }
            return Ok(c);
        }
    }
    Err(KabiPayError::Validation(format!(
        "no active EARNING salary component with code `{}` (or fallback ARREAR) for arrear lines",
        configured_arrear_code
    )))
}

/// Latest TDS to withhold (per month) for each employee from `tax_computation` for the given India FY
/// (when not null). If multiple rows, the most recently `computed_at` row wins.
async fn tds_by_employee_fy(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    employee_ids: &[Uuid],
    fy: i32,
) -> KabiPayResult<HashMap<Uuid, Decimal>> {
    if employee_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = tax_computation::Entity::find()
        .filter(tax_computation::Column::TenantId.eq(tenant_id))
        .filter(tax_computation::Column::EmployeeId.is_in(employee_ids.to_vec()))
        .filter(tax_computation::Column::FiscalYear.eq(fy))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut best: HashMap<Uuid, tax_computation::Model> = HashMap::new();
    for r in rows {
        best.entry(r.employee_id)
            .and_modify(|p| {
                if r.computed_at > p.computed_at {
                    *p = r.clone();
                }
            })
            .or_insert(r);
    }
    Ok(best
        .into_iter()
        .filter_map(|(eid, v)| v.tds_per_month.map(|d| (eid, d)))
        .collect())
}

/// v2 pay run: for each ACTIVE employee without a payslip, insert payslip (BASIC = employment salary,
/// optional `ARREAR` line(s)), India statutory stub (EPF, ESI, PT, TDS from `tax_computation`), mark
/// cycle `PROCESSED`. `DRAFT` only.
pub async fn run_payroll_for_cycle(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    cycle_id: Uuid,
    processed_by: Uuid,
) -> KabiPayResult<payroll_cycle::Model> {
    let cycle_row = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::Id.eq(cycle_id))
        .filter(payroll_cycle::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "payroll_cycle",
            id: cycle_id.to_string(),
        })?;

    if cycle_row.status.to_ascii_uppercase() != "DRAFT" {
        return Err(KabiPayError::Validation(format!(
            "payroll cycle must be DRAFT to run (current status: {})",
            cycle_row.status
        )));
    }

    let comp_cfg = find_payroll_compliance_setting(db, tenant_id).await?;
    let base_code = comp_cfg
        .as_ref()
        .map(|c| c.base_salary_component_code.as_str())
        .unwrap_or("BASIC");
    let arrear_code = comp_cfg
        .as_ref()
        .map(|c| c.arrear_salary_component_code.as_str())
        .unwrap_or("ARREAR");

    let basic_comp = resolve_default_earning_component(db, tenant_id, base_code).await?;

    let txn = db.begin().await.map_err(KabiPayError::from)?;

    let existing_slips = payslip::Entity::find()
        .filter(payslip::Column::TenantId.eq(tenant_id))
        .filter(payslip::Column::PayrollCycleId.eq(cycle_id))
        .all(&txn)
        .await
        .map_err(KabiPayError::from)?;
    let mut have: HashSet<Uuid> = existing_slips.iter().map(|p| p.employee_id).collect();

    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.eq("ACTIVE"))
        .all(&txn)
        .await
        .map_err(KabiPayError::from)?;

    let fy = statutory_india::india_fy_start_year(cycle_row.month, cycle_row.year);
    let emp_id_list: Vec<Uuid> = employees.iter().map(|e| e.id).collect();
    let tds_map = tds_by_employee_fy(&txn, tenant_id, &emp_id_list, fy).await?;

    let now = Utc::now();
    let payroll_period_start = period_start(cycle_row.month, cycle_row.year)?;
    for emp in employees {
        if have.contains(&emp.id) {
            continue;
        }
        let base = latest_employment_salary(&txn, tenant_id, emp.id)
            .await?
            .unwrap_or(Decimal::ZERO);
        let structure_breakup = match active_employee_salary_structure(
            &txn,
            tenant_id,
            emp.id,
            payroll_period_start,
        )
        .await?
        {
            Some(structure) => Some(
                salary_breakup_for_structure(
                    &txn,
                    tenant_id,
                    emp.id,
                    structure,
                    base_code,
                    base,
                )
                .await?,
            ),
            None => None,
        };
        let pending = arrear_service::list_pending_by_employee(&txn, tenant_id, emp.id).await?;
        let arrear_sum: Decimal = pending.iter().map(|a| a.amount).sum();
        let structure_gross = structure_breakup
            .as_ref()
            .map(|b| b.monthly_gross)
            .unwrap_or(Decimal::ZERO);
        let structure_deductions = structure_breakup
            .as_ref()
            .map(|b| b.monthly_deductions)
            .unwrap_or(Decimal::ZERO);
        if base <= Decimal::ZERO && structure_gross <= Decimal::ZERO && arrear_sum <= Decimal::ZERO {
            continue;
        }
        let recurring_gross = if structure_gross > Decimal::ZERO {
            structure_gross
        } else {
            base
        };
        let gross = (recurring_gross + arrear_sum).round_dp(2);
        let tds_m = tds_map.get(&emp.id).map(|d| d.round_dp(2));
        let (stat, tds) = statutory_india::compute(gross, tds_m);
        let total_ded = (structure_deductions
            + statutory_india::employee_deduction_total(&stat, tds))
            .round_dp(2);
        let net = (gross - total_ded).round_dp(2);

        let pid = Uuid::new_v4();

        payslip::ActiveModel {
            id: Set(pid),
            tenant_id: Set(tenant_id),
            employee_id: Set(emp.id),
            payroll_cycle_id: Set(cycle_id),
            gross_salary: Set(gross),
            total_deductions: Set(total_ded),
            net_salary: Set(net),
            pf_employee: Set(Some(stat.pf_employee)),
            pf_employer: Set(Some(stat.pf_employer)),
            esi_employee: Set(Some(stat.esi_employee)),
            esi_employer: Set(Some(stat.esi_employer)),
            tds_amount: Set(Some(tds)),
            professional_tax: Set(Some(stat.professional_tax)),
            uan_number: Set(emp.uan_number.clone()),
            esic_number: Set(emp.esic_number.clone()),
            status: Set("GENERATED".to_string()),
            generated_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(KabiPayError::from)?;

        if let Some(breakup) = &structure_breakup {
            for line in &breakup.lines {
                if line.monthly_amount <= Decimal::ZERO {
                    continue;
                }
                payslip_component::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    tenant_id: Set(tenant_id),
                    payslip_id: Set(pid),
                    salary_component_id: Set(line.salary_component_id),
                    amount: Set(line.monthly_amount),
                    component_type: Set(Some(line.component_type.clone())),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&txn)
                .await
                .map_err(KabiPayError::from)?;
            }
        } else if base > Decimal::ZERO {
            let line_id = Uuid::new_v4();
            payslip_component::ActiveModel {
                id: Set(line_id),
                tenant_id: Set(tenant_id),
                payslip_id: Set(pid),
                salary_component_id: Set(basic_comp.id),
                amount: Set(base),
                component_type: Set(Some(basic_comp.r#type.clone())),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&txn)
            .await
            .map_err(KabiPayError::from)?;
        }
        if arrear_sum > Decimal::ZERO {
            let ac =
                resolve_arrear_salary_component(&txn, tenant_id, arrear_code).await?;
            let line_id = Uuid::new_v4();
            payslip_component::ActiveModel {
                id: Set(line_id),
                tenant_id: Set(tenant_id),
                payslip_id: Set(pid),
                salary_component_id: Set(ac.id),
                amount: Set(arrear_sum),
                component_type: Set(Some(ac.r#type.clone())),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&txn)
            .await
            .map_err(KabiPayError::from)?;
            let a_ids: Vec<Uuid> = pending.iter().map(|a| a.id).collect();
            arrear_service::mark_applied(&txn, tenant_id, &a_ids, cycle_id).await?;
        }

        have.insert(emp.id);
    }

    let mut cycle_am: payroll_cycle::ActiveModel = cycle_row.into();
    cycle_am.status = Set("PROCESSED".to_string());
    cycle_am.processed_at = Set(Some(now));
    cycle_am.processed_by = Set(Some(processed_by));
    cycle_am.updated_at = Set(now);
    let updated = cycle_am.update(&txn).await.map_err(KabiPayError::from)?;

    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(updated)
}
