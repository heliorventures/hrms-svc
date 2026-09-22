//! Program policy loading and immutable launch snapshots.
//!
//! The program row is locked before reading its policy. Policy saves and archive
//! operations acquire that same lock, so a cycle cannot combine an old policy
//! with a newer archive or population selection.

use chrono::{Days, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement, TryGetable};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopulationMode {
    All,
    Departments,
    Locations,
    Employees,
}

impl PopulationMode {
    fn parse(value: &str) -> KabiPayResult<Self> {
        match value {
            "ALL" => Ok(Self::All),
            "DEPARTMENTS" => Ok(Self::Departments),
            "LOCATIONS" => Ok(Self::Locations),
            "EMPLOYEES" => Ok(Self::Employees),
            _ => Err(KabiPayError::Validation(
                "Performance program policy has an invalid population mode".into(),
            )),
        }
    }

    fn eligible_employee_predicate(self) -> &'static str {
        match self {
            // Keep the program bind present for every mode so PostgreSQL sees
            // the same parameter arity for each static predicate.
            Self::All => "$5::uuid IS NOT NULL",
            Self::Departments => {
                "EXISTS (SELECT 1 FROM performance_program_population population WHERE population.tenant_id = e.tenant_id AND population.performance_program_id = $5 AND population.selection_id = e.department_id)"
            }
            Self::Locations => {
                "EXISTS (SELECT 1 FROM performance_program_population population WHERE population.tenant_id = e.tenant_id AND population.performance_program_id = $5 AND population.selection_id = e.location_id)"
            }
            Self::Employees => {
                "EXISTS (SELECT 1 FROM performance_program_population population WHERE population.tenant_id = e.tenant_id AND population.performance_program_id = $5 AND population.selection_id = e.id)"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeadlineOffsets {
    goal_setting: Option<i32>,
    self_review: Option<i32>,
    manager_review: Option<i32>,
    calibration: Option<i32>,
    acknowledgement: Option<i32>,
}

/// The existing manual mutation can still set these two legacy dates when a
/// policy leaves their offsets unset. Every configured policy offset wins and
/// is calculated from the cycle start date.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManualDeadlineOverrides {
    pub self_review_due_date: Option<NaiveDate>,
    pub manager_review_due_date: Option<NaiveDate>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CycleDeadlines {
    pub goal_setting_due_date: Option<NaiveDate>,
    pub self_review_due_date: Option<NaiveDate>,
    pub manager_review_due_date: Option<NaiveDate>,
    pub calibration_due_date: Option<NaiveDate>,
    pub acknowledgement_due_date: Option<NaiveDate>,
}

#[derive(Clone, Debug)]
pub struct LaunchPolicy {
    pub program_status: String,
    population_mode: PopulationMode,
    deadline_offsets: DeadlineOffsets,
}

/// Locks the program row used by archive and policy-save operations before
/// loading the policy that controls the new cycle. A missing policy preserves
/// legacy all-eligible population behavior with no configured deadlines.
pub async fn lock_launch_policy(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    program_id: Uuid,
) -> KabiPayResult<LaunchPolicy> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT p.status AS program_status, COALESCE(policy.population_mode, 'ALL') AS population_mode, policy.goal_setting_due_days, policy.self_review_due_days, policy.manager_review_due_days, policy.calibration_due_days, policy.acknowledgement_due_days FROM performance_program p LEFT JOIN performance_program_policy policy ON policy.tenant_id = p.tenant_id AND policy.performance_program_id = p.id WHERE p.tenant_id = $1 AND p.id = $2 FOR UPDATE OF p",
            [tenant_id.into(), program_id.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance program",
            id: program_id.to_string(),
        })?;
    Ok(LaunchPolicy {
        program_status: row.try_get("", "program_status")?,
        population_mode: PopulationMode::parse(&row.try_get::<String>("", "population_mode")?)?,
        deadline_offsets: DeadlineOffsets {
            goal_setting: row.try_get("", "goal_setting_due_days")?,
            self_review: row.try_get("", "self_review_due_days")?,
            manager_review: row.try_get("", "manager_review_due_days")?,
            calibration: row.try_get("", "calibration_due_days")?,
            acknowledgement: row.try_get("", "acknowledgement_due_days")?,
        },
    })
}

fn deadline_from_offset(start_date: NaiveDate, offset_days: Option<i32>) -> KabiPayResult<Option<NaiveDate>> {
    let Some(offset_days) = offset_days else {
        return Ok(None);
    };
    let offset_days = u64::try_from(offset_days).map_err(|_| {
        KabiPayError::Validation("Performance program policy deadline offsets cannot be negative".into())
    })?;
    start_date
        .checked_add_days(Days::new(offset_days))
        .ok_or_else(|| {
            KabiPayError::Validation(
                "Performance program policy deadline exceeds the supported date range".into(),
            )
        })
        .map(Some)
}

/// Converts the current policy to values persisted on a new review cycle.
/// Existing cycles never call this function, preserving their original
/// deadline snapshot after later policy changes.
pub fn snapshot_cycle_deadlines(
    policy: &LaunchPolicy,
    cycle_start_date: NaiveDate,
    manual: ManualDeadlineOverrides,
) -> KabiPayResult<CycleDeadlines> {
    Ok(CycleDeadlines {
        goal_setting_due_date: deadline_from_offset(cycle_start_date, policy.deadline_offsets.goal_setting)?,
        self_review_due_date: deadline_from_offset(cycle_start_date, policy.deadline_offsets.self_review)?
            .or(manual.self_review_due_date),
        manager_review_due_date: deadline_from_offset(cycle_start_date, policy.deadline_offsets.manager_review)?
            .or(manual.manager_review_due_date),
        calibration_due_date: deadline_from_offset(cycle_start_date, policy.deadline_offsets.calibration)?,
        acknowledgement_due_date: deadline_from_offset(cycle_start_date, policy.deadline_offsets.acknowledgement)?,
    })
}

/// Inserts the immutable employee and reporting snapshots for one new cycle.
/// The caller must have acquired [`lock_launch_policy`] in this transaction so
/// the policy rows used by the static mode predicate cannot change mid-launch.
pub async fn insert_eligible_participants(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    program_id: Uuid,
    cycle_id: Uuid,
    appraisal_template_id: Uuid,
    launched_at: chrono::DateTime<Utc>,
    policy: &LaunchPolicy,
) -> KabiPayResult<u64> {
    let sql = format!(
        r#"INSERT INTO performance_participant
           (id, tenant_id, review_cycle_id, employee_id, manager_employee_id, department_id,
            designation_id, work_location_id, appraisal_template_id, status, is_excluded,
            response_revision, created_at, updated_at)
           SELECT gen_random_uuid(), e.tenant_id, $1, e.id, e.reporting_manager_id, e.department_id,
                  e.designation_id, e.location_id, $2, 'GOAL_SETTING', FALSE, 1, $3, $3
           FROM employee e
           WHERE e.tenant_id = $4 AND e.is_deleted = FALSE
             AND UPPER(TRIM(e.status)) IN ('ACTIVE','PROBATION','ON_LEAVE')
             AND e.user_id IS NOT NULL
             AND {}"#,
        policy.population_mode.eligible_employee_predicate(),
    );
    let result = txn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [
                cycle_id.into(),
                appraisal_template_id.into(),
                launched_at.into(),
                tenant_id.into(),
                program_id.into(),
            ],
        ))
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(offsets: DeadlineOffsets) -> LaunchPolicy {
        LaunchPolicy {
            program_status: "ACTIVE".into(),
            population_mode: PopulationMode::All,
            deadline_offsets: offsets,
        }
    }

    #[test]
    fn configured_offsets_override_manual_dates_and_preserve_manual_gaps() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 1).expect("valid date");
        let deadlines = snapshot_cycle_deadlines(
            &policy(DeadlineOffsets {
                self_review: Some(5),
                acknowledgement: Some(12),
                ..Default::default()
            }),
            start,
            ManualDeadlineOverrides {
                self_review_due_date: Some(NaiveDate::from_ymd_opt(2026, 9, 3).expect("valid date")),
                manager_review_due_date: Some(NaiveDate::from_ymd_opt(2026, 9, 8).expect("valid date")),
            },
        )
        .expect("valid offsets");
        assert_eq!(deadlines.self_review_due_date, NaiveDate::from_ymd_opt(2026, 9, 6));
        assert_eq!(deadlines.manager_review_due_date, NaiveDate::from_ymd_opt(2026, 9, 8));
        assert_eq!(deadlines.acknowledgement_due_date, NaiveDate::from_ymd_opt(2026, 9, 13));
    }

    #[test]
    fn population_modes_target_their_declared_employee_snapshot_fields() {
        assert!(PopulationMode::All
            .eligible_employee_predicate()
            .contains("$5::uuid IS NOT NULL"));
        assert!(PopulationMode::Departments
            .eligible_employee_predicate()
            .contains("e.department_id"));
        assert!(PopulationMode::Locations
            .eligible_employee_predicate()
            .contains("e.location_id"));
        assert!(PopulationMode::Employees
            .eligible_employee_predicate()
            .contains("e.id"));
    }

    #[test]
    fn date_overflow_is_rejected_before_cycle_creation() {
        let result = deadline_from_offset(NaiveDate::MAX, Some(1));
        assert!(result.is_err());
    }
}
