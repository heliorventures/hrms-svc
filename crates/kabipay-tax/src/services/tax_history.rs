use super::tax_settings::{check_revision, lock_employee_tax, require_employee};
use crate::domain::{CoverageStatus, TaxHistoryEntry};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0093_tax_projection_configuration::employee_tax_history as history;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryVersion {
    pub id: Uuid,
    pub revision: i32,
    pub entry: TaxHistoryEntry,
}
pub fn require_no_overlap(
    start: NaiveDate,
    end: NaiveDate,
    other_start: NaiveDate,
    other_end: NaiveDate,
) -> KabiPayResult<()> {
    if start <= other_end && end >= other_start {
        return Err(KabiPayError::Validation(
            "tax history overlaps recorded earnings; split the covered period".into(),
        ));
    }
    Ok(())
}
pub async fn list_tax_history<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
) -> KabiPayResult<Vec<HistoryVersion>> {
    let rows = history::Entity::find()
        .filter(history::Column::TenantId.eq(tenant))
        .filter(history::Column::EmployeeId.eq(employee))
        .filter(history::Column::FiscalYear.eq(year))
        .order_by_desc(history::Column::Revision)
        .all(db)
        .await?;
    let mut seen = BTreeSet::new();
    rows.into_iter()
        .filter(|r| seen.insert(r.source_key.clone()))
        .map(|r| {
            Ok(HistoryVersion {
                id: r.id,
                revision: r.revision,
                entry: serde_json::from_value(r.payload)
                    .map_err(|_| KabiPayError::Internal("invalid stored history".into()))?,
            })
        })
        .collect()
}
pub async fn save_tax_history<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    entry: TaxHistoryEntry,
    expected_revision: Option<i32>,
) -> KabiPayResult<HistoryVersion> {
    entry.validate()?;
    require_employee(db, tenant, employee).await?;
    lock_employee_tax(db, tenant, employee).await?;
    let all = list_tax_history(db, tenant, employee, entry.fiscal_year).await?;
    let current = all.iter().find(|r| r.entry.source_key == entry.source_key);
    if let Some(existing) = current.filter(|r| r.entry == entry) {
        return Ok(existing.clone());
    }
    check_revision(current.map(|v| v.revision), expected_revision)?;
    for other in all
        .iter()
        .filter(|r| r.entry.source_key != entry.source_key && r.entry.employer == entry.employer)
    {
        require_no_overlap(
            entry.period_start,
            entry.period_end,
            other.entry.period_start,
            other.entry.period_end,
        )?;
    }
    if entry.employer == "CURRENT" {
        let overlap = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM payroll_period_input WHERE tenant_id=$1 AND employee_id=$2 AND make_date(year,month,1)<=$4 AND (make_date(year,month,1)+interval '1 month'-interval '1 day')::date>=$3 LIMIT 1",
            [tenant.into(),employee.into(),entry.period_start.into(),entry.period_end.into()])).await?;
        if overlap.is_some() {
            return Err(KabiPayError::Validation(
                "opening history overlaps an imported payroll period".into(),
            ));
        }
        let finalized = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM payslip p JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND c.status IN ('PROCESSED','LOCKED') AND make_date(c.year,c.month,1)<=$4 AND (make_date(c.year,c.month,1)+interval '1 month'-interval '1 day')::date>=$3 LIMIT 1",
            [tenant.into(),employee.into(),entry.period_start.into(),entry.period_end.into()])).await?;
        if finalized.is_some() {
            return Err(KabiPayError::Validation(
                "opening history overlaps finalized payroll".into(),
            ));
        }
    }
    let revision = current.map_or(1, |v| v.revision + 1);
    let payload = serde_json::to_value(&entry)
        .map_err(|_| KabiPayError::Validation("invalid history".into()))?;
    let row = history::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant),
        employee_id: Set(employee),
        fiscal_year: Set(entry.fiscal_year),
        source_key: Set(entry.source_key.clone()),
        revision: Set(revision),
        period_start: Set(entry.period_start),
        period_end: Set(entry.period_end),
        employer: Set(entry.employer.clone()),
        earnings: Set(entry.earnings),
        tds: Set(entry.tds),
        coverage: Set(match entry.coverage {
            CoverageStatus::Complete => "COMPLETE",
            CoverageStatus::Incomplete => "INCOMPLETE",
        }
        .into()),
        payload: Set(payload),
        actor_id: Set(actor),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(db)
    .await?;
    Ok(HistoryVersion {
        id: row.id,
        revision,
        entry,
    })
}
