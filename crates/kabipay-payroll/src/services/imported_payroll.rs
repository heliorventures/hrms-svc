//! Reviewed month inputs use the ordinary locked pay run, never the statutory stub.
use super::payroll_rules::{amount, calculate_period, PeriodInput};
use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0012_payroll::{payslip, payslip_component, salary_component},
    d0090_payroll_period_configuration::{employee_payroll_rule, payslip_statement},
};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

pub fn rule_cutoff(date: NaiveDate) -> KabiPayResult<NaiveDate> {
    Ok(date)
}

pub async fn run<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    cycle: Uuid,
    employee: &employee::Model,
    date: NaiveDate,
) -> KabiPayResult<bool> {
    let period = super::payroll_period_input::find(
        db,
        tenant,
        employee.id,
        date.year(),
        date.month() as i32,
    )
    .await?;
    let rule = employee_payroll_rule::Entity::find()
        .filter(employee_payroll_rule::Column::TenantId.eq(tenant))
        .filter(employee_payroll_rule::Column::EmployeeId.eq(employee.id))
        .filter(employee_payroll_rule::Column::EffectiveFrom.lte(rule_cutoff(date)?))
        .one(db)
        .await?;
    let Some(period) = period else {
        if rule.is_some() {
            return Err(KabiPayError::Validation(
                "imported salary needs reviewed monthly inputs before payroll generation".into(),
            ));
        }
        return Ok(false);
    };
    if !period.ready {
        return Err(KabiPayError::Validation(
            "imported payroll period has unresolved inputs or deduction reasons".into(),
        ));
    }
    let input: PeriodInput = serde_json::from_value(period.input.clone())
        .map_err(|_| KabiPayError::Validation("stored period input is invalid".into()))?;
    let calculation = calculate_period(&input)?;
    if calculation.components.is_empty() {
        return Err(KabiPayError::Validation(
            "earned salary components are required for a payslip".into(),
        ));
    }
    let dated_lwp = super::imported_lwp::validate(db, tenant, employee.id, &input, true).await?;
    if !super::arrear_service::list_pending_by_employee(db, tenant, employee.id)
        .await?
        .is_empty()
    {
        return Err(KabiPayError::Validation(
            "pending arrears require reconciliation with the imported month".into(),
        ));
    }
    let now = Utc::now();
    let id = Uuid::new_v4();
    let stat = |key: &str| amount(calculation.statutory.get(key).map(String::as_str), key);
    let employer = |key: &str| {
        calculation
            .employer
            .get(key)
            .map(|value| amount(Some(value), key))
            .transpose()
    };
    payslip::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant),
        employee_id: Set(employee.id),
        payroll_cycle_id: Set(cycle),
        gross_salary: Set(calculation.gross + calculation.incentive),
        total_deductions: Set(calculation.total_deductions),
        net_salary: Set(calculation.net_earned),
        pf_employee: Set(Some(stat("PF")?)),
        pf_employer: Set(employer("pf")?),
        esi_employee: Set(Some(stat("ESI")?)),
        esi_employer: Set(employer("esi")?),
        tds_amount: Set(Some(stat("TDS")?)),
        professional_tax: Set(Some(stat("PT")?)),
        uan_number: Set(employee.uan_number.clone()),
        esic_number: Set(employee.esic_number.clone()),
        status: Set("GENERATED".into()),
        generated_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?;
    for (code, value) in &calculation.components {
        let component = salary_component::Entity::find()
            .filter(salary_component::Column::TenantId.eq(tenant))
            .filter(salary_component::Column::Code.eq(code))
            .filter(salary_component::Column::IsActive.eq(true))
            .one(db)
            .await?
            .ok_or_else(|| {
                KabiPayError::Validation(
                    "an earned component is missing from the company catalog".into(),
                )
            })?;
        if component.r#type != "EARNING" {
            return Err(KabiPayError::Validation(
                "earned component has an invalid type".into(),
            ));
        }
        payslip_component::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant),
            payslip_id: Set(id),
            salary_component_id: Set(component.id),
            amount: Set(amount(Some(value), "earned component")?),
            component_type: Set(Some("EARNING".into())),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
    }
    let mut statement = serde_json::to_value(&calculation)
        .map_err(|_| KabiPayError::Internal("statement serialization failed".into()))?;
    statement["dated_lwp"] = dated_lwp;
    payslip_statement::ActiveModel {
        payslip_id: Set(id),
        tenant_id: Set(tenant),
        statement: Set(statement),
        period_input_id: Set(Some(period.id)),
        period_input_revision: Set(Some(period.revision)),
        created_at: Set(now),
    }
    .insert(db)
    .await?;
    Ok(true)
}
