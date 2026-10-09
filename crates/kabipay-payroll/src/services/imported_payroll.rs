//! Reviewed month inputs use the ordinary locked pay run, never the statutory stub.
use super::payroll_rules::amount;
use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0012_payroll::{payslip, payslip_component, salary_component},
    d0090_payroll_period_configuration::payslip_statement,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

pub fn rule_cutoff(date: NaiveDate) -> KabiPayResult<NaiveDate> {
    date.with_day(1)
        .and_then(|first| first.checked_add_months(chrono::Months::new(1)))
        .and_then(|next| next.pred_opt())
        .ok_or_else(|| {
            KabiPayError::Validation("payroll month is outside the supported range".into())
        })
}

pub(crate) async fn persist_reviewed<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    cycle: Uuid,
    employee: &employee::Model,
    date: NaiveDate,
    prepared: super::automatic_payroll::PreparedEmployeePayroll,
    loan_snapshot: Option<&super::loan_recovery::PayslipLoanSnapshot>,
) -> KabiPayResult<Uuid> {
    let period = super::payroll_period_input::find(
        db,
        tenant,
        employee.id,
        date.year(),
        date.month() as i32,
    )
    .await?
    .ok_or_else(|| KabiPayError::Validation("reviewed monthly input no longer exists".into()))?;
    let calculation = &prepared.calculation;
    let input = prepared.input;
    if calculation.components.is_empty() {
        return Err(KabiPayError::Validation(
            "earned salary components are required for a payslip".into(),
        ));
    }
    let dated_lwp = super::imported_lwp::validate(db, tenant, employee.id, &input, true).await?;
    super::reviewed_arrears::verify(db, tenant, employee.id, &prepared.arrears).await?;
    super::earned_catalog::validate(db, tenant, calculation).await?;
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
    statement["arrears"] = serde_json::to_value(&prepared.arrears)
        .map_err(|_| KabiPayError::Internal("arrear statement serialization failed".into()))?;
    statement["tax_projection"] = serde_json::to_value(&prepared.tax_projection)
        .map_err(|_| KabiPayError::Internal("tax statement serialization failed".into()))?;
    statement["contribution_evidence"] = serde_json::to_value(&prepared.contribution_evidence)
        .map_err(|_| {
            KabiPayError::Internal("contribution statement serialization failed".into())
        })?;
    statement["contribution_policy"] = serde_json::to_value(&prepared.contribution_policy)
        .map_err(|_| KabiPayError::Internal("contribution policy serialization failed".into()))?;
    statement["payroll_engine_version"] = serde_json::json!("effective-payroll-v2");
    if let Some(snapshot) = loan_snapshot {
        statement["loans"] = serde_json::to_value(snapshot)
            .map_err(|_| KabiPayError::Internal("Loan statement serialization failed".into()))?;
    }
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
    super::arrear_service::mark_applied(
        db,
        tenant,
        &prepared.arrears.iter().map(|r| r.id).collect::<Vec<_>>(),
        cycle,
    )
    .await?;
    Ok(id)
}
