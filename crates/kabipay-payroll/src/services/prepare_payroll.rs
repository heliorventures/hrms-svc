use super::{
    automatic_payroll::{calculate_with_arrears, EmployeePayrollInput, PreparedEmployeePayroll},
    contribution_policy_store,
    payroll_rules::{calculate_period, PeriodInput},
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_tax::domain::{projection::project_payroll_month, tax_year::month_bounds};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

pub async fn prepare<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    input: &PeriodInput,
) -> KabiPayResult<PreparedEmployeePayroll> {
    let arrears = super::reviewed_arrears::pending(db, tenant, employee).await?;
    let arrear_total: rust_decimal::Decimal = arrears.iter().map(|r| r.amount).sum();
    if input.automatic.is_none() {
        let calculation = calculate_period(input)?;
        if !arrears.is_empty()
            && super::payroll_rules::amount(
                calculation.components.get("ARREAR").map(String::as_str),
                "Source ARREAR earnings must reconcile with pending accruals",
            )? != arrear_total
        {
            return Err(KabiPayError::Validation("Source ARREAR earnings must equal the pending accruals; review the source monthly amounts".into()));
        }
        super::earned_catalog::validate(db, tenant, &calculation).await?;
        return Ok(PreparedEmployeePayroll {
            arrears,
            input: input.clone(),
            calculation,
            tax_projection: None,
            contribution_evidence: None,
            contribution_policy: None,
            requires_tax_acknowledgement: false,
        });
    }
    let (start, end) = month_bounds(input.year, input.month as u32)?;
    let policy = contribution_policy_store::effective(db, tenant, start)
        .await?
        .policy;
    let fiscal_year = input.year - i32::from(input.month < 4);
    let mut projection = kabipay_tax::services::tax_projection::load_projection_input(
        db,
        tenant,
        employee,
        fiscal_year,
        input.month as u32,
    )
    .await?;
    // Proration must use the effective company rule, never a previous source month's totals.
    for salary in &mut projection.salaries {
        salary.divisor = Some(policy.lwp_divisor);
    }
    let month = project_payroll_month(
        input.year,
        input.month as u32,
        projection.joining,
        projection.exit,
        &projection.salaries,
    )?;
    let regular = projection
        .salaries
        .iter()
        .filter(|s| s.from <= end && s.until.is_none_or(|until| until >= start))
        .max_by_key(|s| s.from)
        .ok_or_else(|| KabiPayError::Validation("salary is not assigned for this month".into()))?
        .components
        .clone();
    let mut prepared = calculate_with_arrears(
        &EmployeePayrollInput {
            period: input.clone(),
            regular_components: regular,
            month_components: month.components,
            policy,
            projection,
        },
        arrear_total,
    )?;
    prepared.arrears = arrears;
    super::earned_catalog::validate(db, tenant, &prepared.calculation).await?;
    Ok(prepared)
}
