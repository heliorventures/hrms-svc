//! Effective employee contribution eligibility, distinct from monthly exceptions.
use super::contribution_rules::StatutoryEligibility;
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EligibilitySetting {
    pub effective_from: NaiveDate,
    pub eligibility: StatutoryEligibility,
    pub reason: String,
    #[serde(default)]
    pub revision: Option<String>,
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    date: NaiveDate,
) -> KabiPayResult<Option<EligibilitySetting>> {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT rules->'statutory_eligibility' AS value,md5(rules::text) AS revision FROM employee_payroll_rule WHERE tenant_id=$1 AND employee_id=$2 AND effective_from<=$3 AND rules ? 'statutory_eligibility' ORDER BY effective_from DESC LIMIT 1",
        [tenant.into(),employee.into(),date.into()])).await?;
    row.map(|row| {
        let mut setting: EligibilitySetting = serde_json::from_value(row.try_get("", "value")?)
            .map_err(|_| {
                KabiPayError::Validation("stored employee eligibility requires review".into())
            })?;
        setting.revision = Some(row.try_get("", "revision")?);
        Ok(setting)
    })
    .transpose()
}

pub async fn save<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    actor: Uuid,
    mut setting: EligibilitySetting,
) -> KabiPayResult<EligibilitySetting> {
    kabipay_tax::domain::validate_reason(Some(&setting.reason))?;
    if let Some(value) = setting.eligibility.professional_tax {
        kabipay_tax::domain::validate_amount(value)?;
    }
    if setting.effective_from.day() != 1 {
        return Err(KabiPayError::Validation(
            "employee payroll settings must start on the first day of a payroll month".into(),
        ));
    }
    if let Some(value) = setting.eligibility.average_daily_wage {
        kabipay_tax::domain::validate_amount(value)?;
    }
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "LOCK TABLE employee_payroll_rule IN ROW EXCLUSIVE MODE",
    ))
    .await?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        [format!("{tenant}:{employee}:eligibility").into()],
    ))
    .await?;
    let current = find(db, tenant, employee, setting.effective_from).await?;
    if current.as_ref().and_then(|value| value.revision.as_ref()) != setting.revision.as_ref() {
        return Err(KabiPayError::Conflict(
            "employee payroll settings changed; reload before saving".into(),
        ));
    }
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT EXISTS(SELECT 1 FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted) AS employee_exists, EXISTS(SELECT 1 FROM payslip p JOIN payroll_cycle c ON c.id=p.payroll_cycle_id AND c.tenant_id=p.tenant_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND make_date(c.year,c.month,1)>=$3) AS locked",
        [tenant.into(),employee.into(),setting.effective_from.into()])).await?
        .ok_or_else(|| KabiPayError::Internal("cannot validate employee payroll settings".into()))?;
    if !row.try_get::<bool>("", "employee_exists")? || row.try_get::<bool>("", "locked")? {
        return Err(KabiPayError::Validation(
            "employee is unavailable or settings would affect finalized payroll".into(),
        ));
    }
    setting.revision = None;
    let payload = serde_json::to_value(&setting)
        .map_err(|_| KabiPayError::Validation("invalid employee payroll settings".into()))?;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_payroll_rule(tenant_id,employee_id,effective_from,rules,updated_by) VALUES($1,$2,$3,jsonb_build_object('statutory_eligibility',$4::jsonb),$5) ON CONFLICT(tenant_id,employee_id,effective_from) DO UPDATE SET rules=employee_payroll_rule.rules || EXCLUDED.rules,updated_by=EXCLUDED.updated_by,updated_at=NOW()",
        [tenant.into(),employee.into(),setting.effective_from.into(),payload.into(),actor.into()])).await?;
    find(db, tenant, employee, setting.effective_from)
        .await?
        .ok_or_else(|| KabiPayError::Internal("employee payroll settings were not saved".into()))
}

use chrono::Datelike;
