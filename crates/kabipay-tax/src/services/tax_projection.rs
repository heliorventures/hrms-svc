//! Read model: source/finalized actuals replace estimates; missing TDS stays unknown.
use super::{
    tax_history::list_tax_history,
    tax_settings::{effective_tax_settings, require_employee},
};
use crate::domain::{
    projection::{ActualMonth, SalaryPeriod},
    tax_year::month_bounds,
    EvidenceKind, TaxProjectionInput,
};
use chrono::{Datelike, NaiveDate};
use kabipay_common::{salary_breakup::salary_breakup_for_structure, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::{employee_salary_structure, salary_component};
use rust_decimal::Decimal;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, Statement,
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn decimals(value: serde_json::Value) -> KabiPayResult<BTreeMap<String, Decimal>> {
    serde_json::from_value(value).map_err(|_| {
        KabiPayError::Validation("recorded earnings contain unresolved component amounts".into())
    })
}
pub async fn load_projection_input<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
    month: u32,
) -> KabiPayResult<TaxProjectionInput> {
    crate::domain::validate_year(year)?;
    let (as_of, _) = month_bounds(year + if month < 4 { 1 } else { 0 }, month)?;
    let emp = require_employee(db, tenant, employee).await?;
    let settings = effective_tax_settings(db, tenant, employee, as_of)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("HR tax settings are not configured for this period".into())
        })?
        .input;
    let catalog = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant))
        .all(db)
        .await?;
    let taxable_component_codes = catalog
        .iter()
        .filter(|v| v.is_taxable && v.r#type.eq_ignore_ascii_case("EARNING"))
        .map(|v| v.code.clone())
        .collect();
    let assignments = employee_salary_structure::Entity::find()
        .filter(employee_salary_structure::Column::TenantId.eq(tenant))
        .filter(employee_salary_structure::Column::EmployeeId.eq(employee))
        .order_by_asc(employee_salary_structure::Column::EffectiveFrom)
        .all(db)
        .await?;
    let company=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT payload FROM (SELECT payload,effective_until FROM company_payroll_rule WHERE tenant_id=$1 AND effective_from<=$2 ORDER BY effective_from DESC,revision DESC LIMIT 1) latest WHERE effective_until IS NULL OR effective_until>=$2",[tenant.into(),as_of.into()])).await?;
    let divisor = company
        .as_ref()
        .map(|r| r.try_get::<serde_json::Value>("", "payload"))
        .transpose()?
        .and_then(|v| {
            v.get("lwp_divisor")
                .and_then(|d| d.as_u64())
                .and_then(|d| u32::try_from(d).ok())
        });
    let mut salaries = Vec::new();
    for assignment in assignments {
        let from = assignment.effective_from;
        let until = assignment.effective_to;
        let resolved =
            salary_breakup_for_structure(db, tenant, employee, assignment, "BASIC", Decimal::ZERO)
                .await?;
        salaries.push(SalaryPeriod {
            from,
            until,
            divisor,
            components: resolved
                .lines
                .into_iter()
                .filter(|l| l.component_type.eq_ignore_ascii_case("EARNING"))
                .map(|l| (l.component_code, l.monthly_amount))
                .collect(),
        });
    }
    let source_rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT year,month,input FROM payroll_period_input WHERE tenant_id=$1 AND employee_id=$2 AND ready AND ((year=$3 AND month>=4) OR (year=$3+1 AND month<=3))",
        [tenant.into(),employee.into(),year.into()])).await?;
    let mut actuals = BTreeMap::new();
    for row in source_rows {
        let y: i32 = row.try_get("", "year")?;
        let m: i32 = row.try_get("", "month")?;
        let input: serde_json::Value = row.try_get("", "input")?;
        if !input["automatic"].is_null() {
            continue;
        }
        let mut components = decimals(input["expected_earned_components"].clone())?;
        let incentive: Decimal = serde_json::from_value(input["incentive"].clone())
            .map_err(|_| KabiPayError::Validation("source incentive is unresolved".into()))?;
        if !incentive.is_zero() {
            components.insert("INCENTIVE".into(), incentive);
        }
        let tds: Option<Decimal> =
            serde_json::from_value(input["statutory_overrides"]["TDS"].clone())
                .map_err(|_| KabiPayError::Validation("source TDS is invalid".into()))?;
        actuals.insert(
            (y, m),
            ActualMonth {
                year: y,
                month: m as u32,
                components,
                tds,
                evidence: EvidenceKind::ImportedActual,
            },
        );
    }
    let final_rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT p.id,c.year,c.month,p.tds_amount FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND c.status IN ('PROCESSED','LOCKED') AND ((c.year=$3 AND c.month>=4) OR (c.year=$3+1 AND c.month<=3))",
        [tenant.into(),employee.into(),year.into()])).await?;
    for row in final_rows {
        let id: Uuid = row.try_get("", "id")?;
        let y: i32 = row.try_get("", "year")?;
        let m: i32 = row.try_get("", "month")?;
        let lines=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT sc.code,pc.amount FROM payslip_component pc JOIN salary_component sc ON sc.tenant_id=pc.tenant_id AND sc.id=pc.salary_component_id WHERE pc.tenant_id=$1 AND pc.payslip_id=$2 AND sc.type='EARNING'",[tenant.into(),id.into()])).await?;
        let mut components = lines
            .into_iter()
            .map(|r| {
                Ok((
                    r.try_get::<String>("", "code")?,
                    r.try_get::<Decimal>("", "amount")?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, sea_orm::DbErr>>()?;
        let statement = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT statement FROM payslip_statement WHERE tenant_id=$1 AND payslip_id=$2",
                [tenant.into(), id.into()],
            ))
            .await?;
        if let Some(statement) = statement {
            let value: serde_json::Value = statement.try_get("", "statement")?;
            let incentive: Decimal =
                serde_json::from_value(value["incentive"].clone()).map_err(|_| {
                    KabiPayError::Validation("finalized incentive evidence is invalid".into())
                })?;
            if !incentive.is_zero() {
                components.insert("INCENTIVE".into(), incentive);
            }
        }
        actuals.insert(
            (y, m),
            ActualMonth {
                year: y,
                month: m as u32,
                components,
                tds: row.try_get("", "tds_amount")?,
                evidence: EvidenceKind::FinalizedPayroll,
            },
        );
    }
    let history = list_tax_history(db, tenant, employee, year).await?;
    let previous: Vec<_> = history
        .iter()
        .filter(|v| v.entry.employer != "CURRENT")
        .collect();
    let previous_employer_earnings = previous.iter().map(|v| v.entry.earnings).sum();
    let previous_employer_history_complete = previous.iter().all(|v| {
        v.entry.coverage == crate::domain::CoverageStatus::Complete && v.entry.tds.is_some()
    });
    let previous_employer_tds = if previous.iter().all(|v| v.entry.tds.is_some()) {
        Some(previous.iter().filter_map(|v| v.entry.tds).sum())
    } else {
        None
    };
    let approved_deductions = if settings.regime == crate::domain::TaxRegime::Old {
        super::approved_deductions::old_regime_total(db, tenant, employee, year).await?
    } else {
        Decimal::ZERO
    };
    let exit_row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COALESCE(imported_last_working_date,imported_exit_date) AS exit_date FROM employee WHERE tenant_id=$1 AND id=$2",[tenant.into(),employee.into()])).await?;
    let exit = exit_row
        .map(|r| r.try_get::<Option<NaiveDate>>("", "exit_date"))
        .transpose()?
        .flatten();
    let (_, end) = month_bounds(year + 1, 3)?;
    let age = emp.date_of_birth.and_then(|birth| {
        u32::try_from(
            end.year()
                - birth.year()
                - i32::from((end.month(), end.day()) < (birth.month(), birth.day())),
        )
        .ok()
    });
    Ok(TaxProjectionInput {
        fiscal_year: year,
        joining: emp.date_of_joining,
        exit,
        as_of,
        salaries,
        actuals: actuals.into_values().collect(),
        resident: settings.resident,
        settings,
        age_at_year_end: age,
        approved_deductions,
        previous_employer_earnings,
        previous_employer_tds,
        previous_employer_history_complete: Some(previous_employer_history_complete),
        taxable_component_codes,
        opening_history: history
            .into_iter()
            .filter(|v| v.entry.employer == "CURRENT")
            .map(|v| v.entry)
            .collect(),
    })
}
