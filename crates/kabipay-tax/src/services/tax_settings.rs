use crate::domain::{TaxSettings, TaxSettingsInput};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0093_tax_projection_configuration::employee_tax_settings as settings,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, Set, Statement,
};
use uuid::Uuid;

pub fn check_revision(current: Option<i32>, expected: Option<i32>) -> KabiPayResult<()> {
    if current != expected {
        return Err(KabiPayError::Validation(
            "tax settings or history changed; refresh before saving".into(),
        ));
    }
    Ok(())
}
pub async fn require_employee<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
) -> KabiPayResult<employee::Model> {
    employee::Entity::find_by_id(id)
        .filter(employee::Column::TenantId.eq(tenant))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: id.to_string(),
        })
}
/// Call within the caller's transaction; shared with declarations and finalization.
pub async fn lock_employee_tax<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
) -> KabiPayResult<()> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        [format!("{tenant}:{employee}:tax").into()],
    ))
    .await?;
    Ok(())
}
fn decode(row: settings::Model) -> KabiPayResult<TaxSettings> {
    Ok(TaxSettings {
        id: row.id,
        employee_id: row.employee_id,
        revision: row.revision,
        input: serde_json::from_value(row.payload)
            .map_err(|_| KabiPayError::Internal("invalid stored tax settings".into()))?,
    })
}
pub async fn save_tax_settings<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    input: TaxSettingsInput,
    expected_revision: Option<i32>,
) -> KabiPayResult<TaxSettings> {
    input.validate()?;
    require_employee(db, tenant, employee).await?;
    lock_employee_tax(db, tenant, employee).await?;
    let current = settings::Entity::find()
        .filter(settings::Column::TenantId.eq(tenant))
        .filter(settings::Column::EmployeeId.eq(employee))
        .order_by_desc(settings::Column::Revision)
        .one(db)
        .await?;
    check_revision(current.as_ref().map(|v| v.revision), expected_revision)?;
    let revision = current.map_or(1, |v| v.revision + 1);
    let row = settings::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant),
        employee_id: Set(employee),
        revision: Set(revision),
        effective_from: Set(input.effective_from),
        effective_until: Set(input.effective_until),
        payload: Set(serde_json::to_value(&input)
            .map_err(|_| KabiPayError::Validation("invalid tax settings".into()))?),
        actor_id: Set(actor),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(db)
    .await?;
    decode(row)
}
pub async fn list_tax_settings<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
) -> KabiPayResult<Vec<TaxSettings>> {
    settings::Entity::find()
        .filter(settings::Column::TenantId.eq(tenant))
        .filter(settings::Column::EmployeeId.eq(employee))
        .order_by_desc(settings::Column::Revision)
        .all(db)
        .await?
        .into_iter()
        .map(decode)
        .collect()
}
pub async fn effective_tax_settings<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    as_of: NaiveDate,
) -> KabiPayResult<Option<TaxSettings>> {
    Ok(kabipay_common::effective_version::effective(
        list_tax_settings(db, tenant, employee).await?,
        as_of,
        |r| (r.input.effective_from, r.revision, r.input.effective_until),
    ))
}
