use super::EmployeeState;
use kabipay_common::db::resolve_required_tenant_db;
use kabipay_db_entities::ops::tenant_database;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::{sync::Arc, time::Duration};

pub(super) async fn run(state: Arc<EmployeeState>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if kabipay_common::finance_contract::service_secret().is_err() {
            continue;
        }
        let mut page = 0;
        loop {
            let rows = match tenant_database::Entity::find()
                .filter(tenant_database::Column::IsActive.eq(true))
                .order_by_asc(tenant_database::Column::Id)
                .offset(page * 100)
                .limit(100)
                .all(&state.ops)
                .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    tracing::warn!("Private Finance worker cannot list tenant connections");
                    break;
                }
            };
            let more = rows.len() == 100;
            for row in rows {
                let db = match resolve_required_tenant_db(
                    row.tenant_id,
                    &state.ops,
                    &state.cache,
                    &state.fallback,
                )
                .await
                {
                    Ok(db) => db,
                    Err(_) => {
                        tracing::warn!("Private Finance worker cannot resolve a tenant connection");
                        continue;
                    }
                };
                if kabipay_employee::services::finance_outbox::deliver_due(&db, row.tenant_id)
                    .await
                    .is_err()
                {
                    tracing::warn!("Private Finance delivery requires reconciliation");
                }
            }
            if !more {
                break;
            }
            page += 1;
        }
    }
}
