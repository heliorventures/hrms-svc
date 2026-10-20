//! Source-owned idle liveness and resumable expanded metric snapshots.
//! Root startup calls this with an already bound company connection. It never
//! accepts another company's connection string or emits worker uptime as health.
use crate::statement;
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait,DatabaseConnection,TransactionTrait};
use uuid::Uuid;
pub async fn capture_once(db:&DatabaseConnection,tenant:Uuid)->KabiPayResult<()> {
 let tx=db.begin().await?;
 tx.execute(statement("SELECT organization_metric_bootstrap_once($1,100)",vec![tenant.into()])).await?;
 tx.execute(statement("SELECT organization_capture_tick($1)",vec![tenant.into()])).await?;
 tx.commit().await?;
 Ok(())
}
