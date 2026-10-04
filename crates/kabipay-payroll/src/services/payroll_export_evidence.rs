//! Reconcile recorded current-employer evidence without treating estimates as paid.
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Default)]
pub struct Supplement {
    pub gross: Decimal,
    pub deductions: Decimal,
    pub net: Decimal,
    pub tds: Decimal,
    pub pf: Decimal,
    pub esi: Decimal,
    pub pt: Decimal,
    pub history_count: usize,
    pub source_count: usize,
    pub unknown_tds: usize,
}

pub async fn supplements<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    year: i32,
    quarter: Option<i32>,
) -> KabiPayResult<HashMap<Uuid, Supplement>> {
    kabipay_tax::domain::validate_year(year)?;
    if quarter.is_some_and(|q| !(1..=4).contains(&q)) {
        return Err(KabiPayError::Validation(
            "Export quarter must be 1 to 4".into(),
        ));
    }
    let months = (0..12)
        .map(|offset| {
            let month = (offset + 3) % 12 + 1;
            (year + i32::from(month < 4), month)
        })
        .collect::<Vec<_>>();
    let selected = months
        .iter()
        .enumerate()
        .filter(|(i, _)| quarter.is_none_or(|q| *i / 3 == (q - 1) as usize))
        .map(|(_, m)| *m)
        .collect::<Vec<_>>();
    let (first_year, first_month) = *selected
        .first()
        .ok_or_else(|| KabiPayError::Validation("Invalid export quarter".into()))?;
    let (last_year, last_month) = *selected.last().unwrap();
    let start = kabipay_tax::domain::tax_year::month_bounds(first_year, first_month)?.0;
    let end = kabipay_tax::domain::tax_year::month_bounds(last_year, last_month)?.1;
    let mut result: HashMap<Uuid, Supplement> = HashMap::new();
    let employees = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT DISTINCT employee_id FROM employee_tax_history WHERE tenant_id=$1 AND fiscal_year=$2", [tenant.into(),year.into()])).await?;
    for row in employees {
        let employee: Uuid = row.try_get("", "employee_id")?;
        for version in
            kabipay_tax::services::tax_history::list_tax_history(db, tenant, employee, year).await?
        {
            let entry = version.entry;
            if entry.employer != "CURRENT" || entry.period_end < start || entry.period_start > end {
                continue;
            }
            if entry.period_start < start || entry.period_end > end {
                return Err(KabiPayError::Validation("Opening history spans multiple quarters. HR must supply separate period amounts before a quarterly export; the annual export includes the full recorded amount.".into()));
            }
            let overlap = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM payroll_period_input WHERE tenant_id=$1 AND employee_id=$2 AND ready AND (input->'automatic' IS NULL OR input->'automatic'='null'::jsonb) AND make_date(year,month,1)<=$4 AND (make_date(year,month,1)+INTERVAL '1 month - 1 day')::date>=$3 UNION ALL SELECT 1 FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND c.status IN ('PROCESSED','LOCKED') AND make_date(c.year,c.month,1)<=$4 AND (make_date(c.year,c.month,1)+INTERVAL '1 month - 1 day')::date>=$3) AS overlap",
                [tenant.into(),employee.into(),entry.period_start.into(),entry.period_end.into()])).await?
                .ok_or_else(|| KabiPayError::Internal("Export evidence check unavailable".into()))?;
            if overlap.try_get::<bool>("", "overlap")? {
                return Err(KabiPayError::Validation("Opening history overlaps monthly payroll evidence; reconcile the recorded periods before exporting".into()));
            }
            let total = result.entry(employee).or_default();
            total.gross += entry.earnings;
            total.tds += entry.tds.unwrap_or_default();
            total.unknown_tds += usize::from(entry.tds.is_none());
            total.history_count += 1;
        }
    }
    let sources = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT i.employee_id,i.year,i.month,i.input FROM payroll_period_input i WHERE i.tenant_id=$1 AND i.ready AND make_date(i.year,i.month,1) BETWEEN $2 AND $3 AND NOT EXISTS (SELECT 1 FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE p.tenant_id=i.tenant_id AND p.employee_id=i.employee_id AND c.year=i.year AND c.month=i.month AND c.status IN ('PROCESSED','LOCKED'))",
        [tenant.into(),start.into(),end.into()])).await?;
    for row in sources {
        let input: super::payroll_rules::PeriodInput =
            serde_json::from_value(row.try_get("", "input")?)
                .map_err(|_| KabiPayError::Validation("Recorded export input is invalid".into()))?;
        if input.automatic.is_some() {
            continue;
        }
        let value = super::payroll_rules::calculate_period(&input)?;
        let total = result.entry(row.try_get("", "employee_id")?).or_default();
        total.gross += value.gross + value.incentive;
        total.deductions += value.total_deductions;
        total.net += value.net_earned;
        let stat = |code| {
            super::payroll_rules::amount(
                value.statutory.get(code).map(String::as_str),
                "Recorded deduction",
            )
        };
        total.tds += stat("TDS")?;
        total.pf += stat("PF")?;
        total.esi += stat("ESI")?;
        total.pt += stat("PT")?;
        total.source_count += 1;
    }
    Ok(result)
}
