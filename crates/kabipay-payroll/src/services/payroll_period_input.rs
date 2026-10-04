//! Transaction-aware period writes shared by HR and the importer.
use super::payroll_rules::PeriodInput;
use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::{payroll_cycle, payslip};
use kabipay_db_entities::tenant::d0090_payroll_period_configuration::payroll_period_input;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set,
    Statement,
};
use uuid::Uuid;

pub async fn find<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
    month: i32,
) -> KabiPayResult<Option<payroll_period_input::Model>> {
    Ok(payroll_period_input::Entity::find()
        .filter(payroll_period_input::Column::TenantId.eq(tenant))
        .filter(payroll_period_input::Column::EmployeeId.eq(employee))
        .filter(payroll_period_input::Column::Year.eq(year))
        .filter(payroll_period_input::Column::Month.eq(month))
        .one(db)
        .await?)
}

/// Caller owns the transaction. Reserve the write lock before checking cycle status;
/// finalization's input-table lock must wait for this whole edit (or finish first).
pub async fn save<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    input: PeriodInput,
    source_ref: Option<serde_json::Value>,
    expected_revision: Option<i32>,
) -> KabiPayResult<payroll_period_input::Model> {
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "LOCK TABLE payroll_period_input IN ROW EXCLUSIVE MODE",
    ))
    .await?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        [format!("payroll-period:{tenant}:{}:{}", input.year, input.month).into()],
    ))
    .await?;
    let cycle = payroll_cycle::Entity::find()
        .filter(payroll_cycle::Column::TenantId.eq(tenant))
        .filter(payroll_cycle::Column::Year.eq(input.year))
        .filter(payroll_cycle::Column::Month.eq(input.month))
        .one(db)
        .await?;
    if let Some(cycle) = cycle {
        if cycle.status != "DRAFT"
            || payslip::Entity::find()
                .filter(payslip::Column::TenantId.eq(tenant))
                .filter(payslip::Column::PayrollCycleId.eq(cycle.id))
                .one(db)
                .await?
                .is_some()
        {
            return Err(KabiPayError::Validation(
                "processed or generated periods cannot be edited".into(),
            ));
        }
    }
    let prepared = super::prepare_payroll::prepare(db, tenant, employee, &input).await;
    let ready = if let Ok(prepared) = &prepared {
        input.ready
            && super::imported_lwp::validate(db, tenant, employee, &prepared.input, false)
                .await
                .is_ok()
    } else {
        false
    };
    // Retain the resolved month used by payroll for read-only locked-period review.
    // Automatic recalculation still resolves effective settings and approved leave afresh.
    let stored_input = prepared.as_ref().map(|value| &value.input).unwrap_or(&input);
    let value = serde_json::to_value(stored_input)
        .map_err(|_| KabiPayError::Validation("invalid period input".into()))?;
    let existing = find(db, tenant, employee, input.year, input.month).await?;
    if let Some(row) = &existing {
        if row.input == value && row.ready == ready {
            return Ok(row.clone());
        }
        if expected_revision != Some(row.revision) {
            return Err(KabiPayError::Validation(
                "period input changed; reload before saving".into(),
            ));
        }
    } else if expected_revision.is_some() {
        return Err(KabiPayError::Validation(
            "period input no longer exists".into(),
        ));
    }
    let now = Utc::now();
    let (id, revision, created) = existing
        .as_ref()
        .map(|row| (row.id, row.revision + 1, row.created_at))
        .unwrap_or((Uuid::new_v4(), 1, now));
    let model = payroll_period_input::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant),
        employee_id: Set(employee),
        year: Set(input.year),
        month: Set(input.month),
        input: Set(value.clone()),
        ready: Set(ready),
        source_ref: Set(
            source_ref.or_else(|| existing.as_ref().and_then(|row| row.source_ref.clone()))
        ),
        revision: Set(revision),
        updated_by: Set(actor),
        created_at: Set(created),
        updated_at: Set(now),
    };
    let result = if existing.is_some() {
        model.update(db).await?
    } else {
        model.insert(db).await?
    };
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO payroll_period_input_audit(tenant_id,period_input_id,revision,input,changed_by) VALUES($1,$2,$3,$4,$5)",
        [tenant.into(),id.into(),revision.into(),value.into(),actor.into()])).await?;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM payroll_period_adjustment WHERE tenant_id=$1 AND employee_id=$2 AND year=$3 AND month=$4",
        [tenant.into(),employee.into(),input.year.into(),input.month.into()])).await?;
    let mut codes = std::collections::HashSet::new();
    for item in input.additional_deductions {
        if !super::payroll_rules::valid_additional_code(&item.code)
            || !codes.insert(item.code.clone())
        {
            continue;
        }
        super::component_display::ensure_additional_deduction(db, tenant, &item.code).await?;
        let Ok(amount) =
            super::payroll_rules::amount(item.amount.as_deref(), "additional deduction")
        else {
            continue;
        };
        let item_ready =
            amount.is_zero() || item.reason.as_deref().is_some_and(|s| !s.trim().is_empty());
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO payroll_period_adjustment(tenant_id,employee_id,year,month,code,amount,reason,ready,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            [tenant.into(),employee.into(),input.year.into(),input.month.into(),item.code.into(),amount.into(),item.reason.into(),item_ready.into(),actor.into()])).await?;
    }
    Ok(result)
}
