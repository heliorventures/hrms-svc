use super::contribution_rules::ContributionPolicy;
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0093_tax_projection_configuration::company_payroll_rule as rule;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, Set, Statement,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyVersion {
    pub id: Uuid,
    pub revision: i32,
    pub policy: ContributionPolicy,
}
fn decode(row: rule::Model) -> KabiPayResult<PolicyVersion> {
    Ok(PolicyVersion {
        id: row.id,
        revision: row.revision,
        policy: serde_json::from_value(row.payload).map_err(|_| {
            KabiPayError::Internal("invalid stored company contribution policy".into())
        })?,
    })
}
pub async fn list<C: ConnectionTrait>(db: &C, tenant: Uuid) -> KabiPayResult<Vec<PolicyVersion>> {
    rule::Entity::find()
        .filter(rule::Column::TenantId.eq(tenant))
        .order_by_desc(rule::Column::Revision)
        .all(db)
        .await?
        .into_iter()
        .map(decode)
        .collect()
}
pub async fn effective<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    date: NaiveDate,
) -> KabiPayResult<PolicyVersion> {
    kabipay_common::effective_version::effective(list(db, tenant).await?, date, |r| {
        (
            r.policy.effective_from,
            r.revision,
            r.policy.effective_until,
        )
    })
    .ok_or_else(|| {
        KabiPayError::Validation("company payroll rules are not configured for this month".into())
    })
}
pub async fn save<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    policy: ContributionPolicy,
    expected_revision: Option<i32>,
) -> KabiPayResult<PolicyVersion> {
    policy.validate()?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        [format!("{tenant}:company-payroll-rule").into()],
    ))
    .await?;
    let current = list(db, tenant).await?;
    if let Some(same) = current
        .first()
        .filter(|r| serde_json::to_value(&r.policy).ok() == serde_json::to_value(&policy).ok())
    {
        return Ok(same.clone());
    }
    kabipay_tax::services::tax_settings::check_revision(
        current.first().map(|v| v.revision),
        expected_revision,
    )?;
    let row = rule::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant),
        revision: Set(current.first().map_or(1, |r| r.revision + 1)),
        effective_from: Set(policy.effective_from),
        effective_until: Set(policy.effective_until),
        reason: Set(policy.reason.clone()),
        payload: Set(serde_json::to_value(policy)
            .map_err(|_| KabiPayError::Validation("invalid company policy".into()))?),
        actor_id: Set(actor),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(db)
    .await?;
    decode(row)
}
