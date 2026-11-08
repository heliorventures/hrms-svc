//! Expired guards remain held until Employee supplies an authoritative terminal outcome.
use super::{
    coordination,
    http::{hr_read, PayrollSchema},
};
use kabipay_common::{
    db::{resolve_required_tenant_db, TenantDbCache, TenantDbConfig},
    finance_contract::{ExitOperationStatus, FinanceAction},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::ops::tenant_database;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub fn start(schema: Arc<PayrollSchema>) {
    tokio::spawn(run(schema));
}

async fn reconcile_due(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<()> {
    let tx = db.begin().await?;
    let pending = kabipay_loans::pending_exit_approvals(&tx, tenant, 5)
        .await
        .map_err(coordination::financial_error)?;
    tx.commit().await?;
    for expected in pending {
        let operation = hr_read(
            tenant,
            FinanceAction::OperationStatus,
            serde_json::json!({"operationId":expected.operation_id}),
        )
        .await
        .ok()
        .and_then(|value| serde_json::from_value::<ExitOperationStatus>(value).ok());
        match operation {
            Some(operation) if coordination::registered(&operation) == expected => {
                coordination::apply(
                    db,
                    tenant,
                    None,
                    FinanceAction::ConfirmExitApproval,
                    &operation,
                )
                .await?;
            }
            _ => {
                let tx = db.begin().await?;
                kabipay_loans::reconcile_exit_approval(&tx, tenant, &expected, None)
                    .await
                    .map_err(coordination::financial_error)?;
                tx.commit().await?;
            }
        }
    }
    Ok(())
}

async fn run(schema: Arc<PayrollSchema>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if sweep(&schema).await.is_err() {
            tracing::warn!("Private Finance reconciliation requires review");
        }
    }
}

async fn sweep(schema: &PayrollSchema) -> KabiPayResult<()> {
    let ops = schema
        .data::<DatabaseConnection>()
        .ok_or_else(|| KabiPayError::Internal("Finance database unavailable".into()))?;
    let cache = schema
        .data::<TenantDbCache>()
        .ok_or_else(|| KabiPayError::Internal("Finance database cache unavailable".into()))?;
    let fallback = schema.data::<TenantDbConfig>().ok_or_else(|| {
        KabiPayError::Internal("Finance database configuration unavailable".into())
    })?;
    let mut page = 0;
    loop {
        let rows = tenant_database::Entity::find()
            .filter(tenant_database::Column::IsActive.eq(true))
            .order_by_asc(tenant_database::Column::Id)
            .offset(page * 100)
            .limit(100)
            .all(ops)
            .await?;
        let more = rows.len() == 100;
        for row in rows {
            match resolve_required_tenant_db(row.tenant_id, ops, cache, fallback).await {
                Ok(db) => {
                    if crate::services::loan_reconciliation::reconcile_due(&db, row.tenant_id)
                        .await
                        .is_err()
                    {
                        tracing::warn!("Loan financial reconciliation requires review");
                    }
                    if kabipay_common::finance_contract::service_secret().is_ok()
                        && reconcile_due(&db, row.tenant_id).await.is_err()
                    {
                        tracing::warn!("A private exit approval remains pending reconciliation");
                    }
                }
                Err(_) => tracing::warn!("Private Finance cannot resolve a tenant connection"),
            }
        }
        if !more {
            return Ok(());
        }
        page += 1;
    }
}
