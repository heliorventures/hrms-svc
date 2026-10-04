use super::{payroll_draft::DraftEmployee, payroll_rules::PeriodInput};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::payroll_cycle;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

fn period_eligibility(
    status: &str,
    excluded: bool,
    joining: NaiveDate,
    exit: Option<NaiveDate>,
    start: NaiveDate,
    end: NaiveDate,
) -> (&'static str, Option<&'static str>) {
    if excluded {
        return ("EXCLUDED", Some("Excluded from payroll"));
    }
    if exit.is_some_and(|date| date < joining) {
        return ("REVIEW", Some("Exit date precedes joining date"));
    }
    if joining > end || exit.is_some_and(|date| date < start) {
        return (
            "EXCLUDED",
            Some("Outside employment for this payroll month"),
        );
    }
    if !matches!(status, "ACTIVE" | "PROBATION") && exit.is_none() {
        return (
            "REVIEW",
            Some("Confirm the last working date for this employee"),
        );
    }
    ("ELIGIBLE", None)
}

pub async fn employees<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    cycle: &payroll_cycle::Model,
) -> KabiPayResult<Vec<DraftEmployee>> {
    let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,status,payroll_excluded,date_of_joining,COALESCE(imported_last_working_date,imported_exit_date) AS exit_date,concat_ws(' · ',employee_code,concat_ws(' ',first_name,last_name)) AS label FROM employee WHERE tenant_id=$1 AND NOT is_deleted ORDER BY employee_code,id", [tenant.into()])).await?;
    let (start, end) = kabipay_tax::domain::tax_year::month_bounds(cycle.year, cycle.month as u32)?;
    let mut results = Vec::new();
    for row in rows {
        let employee = row.try_get::<Uuid>("", "id")?;
        let employee_label = row.try_get::<String>("", "label")?;
        let excluded = row.try_get::<bool>("", "payroll_excluded")?;
        let status = row.try_get::<String>("", "status")?;
        let (eligibility, reason) = period_eligibility(
            &status,
            excluded,
            row.try_get("", "date_of_joining")?,
            row.try_get("", "exit_date")?,
            start,
            end,
        );
        if eligibility != "ELIGIBLE" {
            results.push(DraftEmployee {
                employee_id: employee,
                employee_label,
                outcome: eligibility.into(),
                reason: reason.map(str::to_owned),
                prepared: None,
            });
            continue;
        }
        let outcome = prepare_employee(db, tenant, actor, employee, cycle).await;
        results.push(match outcome {
            Ok(prepared) => DraftEmployee {
                employee_id: employee,
                employee_label,
                outcome: "READY".into(),
                reason: None,
                prepared: Some(prepared),
            },
            Err(error) => DraftEmployee {
                employee_id: employee,
                employee_label,
                outcome: "REVIEW".into(),
                reason: Some(review_reason(error)),
                prepared: None,
            },
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eligibility_uses_employment_dates_and_keeps_uncertain_exits_for_review() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let check = |status, joining, exit: Option<&str>| {
            period_eligibility(
                status,
                false,
                d(joining),
                exit.map(d),
                d("2026-09-01"),
                d("2026-09-30"),
            )
            .0
        };
        assert_eq!(check("PROBATION", "2026-08-01", None), "ELIGIBLE");
        assert_eq!(
            check("INACTIVE", "2025-01-01", Some("2026-09-15")),
            "ELIGIBLE"
        );
        assert_eq!(
            check("EXITED", "2025-01-01", Some("2026-08-31")),
            "EXCLUDED"
        );
        assert_eq!(check("ACTIVE", "2026-10-01", None), "EXCLUDED");
        assert_eq!(check("INACTIVE", "2025-01-01", None), "REVIEW");
    }
}

fn review_reason(error: KabiPayError) -> String {
    match error {
        KabiPayError::Validation(message) | KabiPayError::Conflict(message) => message,
        _ => "Unable to calculate this employee. Check configuration and contact support if the issue continues.".into(),
    }
}

async fn prepare_employee<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    cycle: &payroll_cycle::Model,
) -> KabiPayResult<super::automatic_payroll::PreparedEmployeePayroll> {
    let existing =
        super::payroll_period_input::find(db, tenant, employee, cycle.year, cycle.month).await?;
    let mut input: PeriodInput = match &existing {
        Some(period) => serde_json::from_value(period.input.clone())
            .map_err(|_| KabiPayError::Validation("Stored monthly input is invalid".into()))?,
        None => super::automatic_period::new_input(cycle.year, cycle.month)?,
    };
    let period = if input.automatic.is_some() {
        input.ready = true;
        super::payroll_period_input::save(
            db,
            tenant,
            actor,
            employee,
            input.clone(),
            None,
            existing.as_ref().map(|row| row.revision),
        )
        .await?
    } else {
        existing
            .ok_or_else(|| KabiPayError::Validation("Reviewed source input is missing".into()))?
    };
    if !period.ready {
        // Re-evaluate to return the precise missing setting, rather than a stale readiness flag.
        super::prepare_payroll::prepare(db, tenant, employee, &input).await?;
        return Err(KabiPayError::Validation(
            "Monthly payroll inputs require review".into(),
        ));
    }
    let prepared = super::prepare_payroll::prepare(db, tenant, employee, &input).await?;
    if prepared.calculation.components.is_empty() {
        return Err(KabiPayError::Validation(
            "Earned salary components are required".into(),
        ));
    }
    super::imported_lwp::validate(db, tenant, employee, &prepared.input, false).await?;
    Ok(prepared)
}
