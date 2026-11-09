//! Restart-safe source execution with fenced claims and atomic business outcome.
use crate::{
    organization_jobs::{DraftInput, FinalizeInput},
    services::{payroll_actor::PayrollActor, payroll_draft, payroll_finalize},
};
use kabipay_common::{
    db::{TenantDbCache, TenantDbConfig},
    KabiPayError, KabiPayResult,
};
use kabipay_organization_protocol::{OrganizationOperation, RuntimeAuthorization};
use kabipay_organization_tenant::{statement, unavailable};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

const LEASE_SECONDS: i64 = 300;
const MAX_ATTEMPTS: i32 = 20;
type PayrollSchema = async_graphql::Schema<
    crate::resolvers::QueryRoot,
    crate::resolvers::MutationRoot,
    async_graphql::EmptySubscription,
>;
pub fn start(schema: Arc<PayrollSchema>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if sweep(&schema).await.is_err() {
                tracing::warn!("Organization payroll job sweep unavailable");
            }
        }
    });
}
async fn sweep(schema: &PayrollSchema) -> KabiPayResult<()> {
    // Each company retains its own job claims, source transaction and outcome.
    let ops = schema
        .data::<DatabaseConnection>()
        .ok_or_else(unavailable)?;
    let cache = schema.data::<TenantDbCache>().ok_or_else(unavailable)?;
    if !cache.is_company_runtime() {
        return Ok(());
    }
    let fallback = schema.data::<TenantDbConfig>().ok_or_else(unavailable)?;
    for tenant in cache.configured_tenant_ids() {
    let result = async {
    let db = kabipay_common::db::resolve_required_tenant_db(tenant, ops, cache, fallback).await?;
    for _ in 0..5 {
        if !run_one(&db, ops, tenant).await? {
            break;
        }
    }
    Ok::<_, KabiPayError>(())
    }.await;
    if let Err(error) = result { tracing::warn!(%tenant,code=error.code(),"Organization payroll company sweep deferred"); }
    }
    Ok(())
}

/// A committed claim contains no authority token and is recoverable after expiry.
pub async fn claim(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<Option<(Uuid, Uuid)>> {
    let token = Uuid::new_v4();
    let row=db.query_one(statement("WITH candidate AS (SELECT id FROM organization_payroll_job WHERE tenant_id=$1 AND next_attempt_at<=clock_timestamp() AND (state='QUEUED' OR (state='RUNNING' AND lease_expires_at<=clock_timestamp())) ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE organization_payroll_job j SET state='RUNNING',lease_token=$2,lease_expires_at=clock_timestamp()+make_interval(secs=>$3::double precision),attempts=attempts+1,started_at=COALESCE(started_at,clock_timestamp()) FROM candidate c WHERE j.tenant_id=$1 AND j.id=c.id RETURNING j.id",vec![tenant.into(),token.into(),LEASE_SECONDS.into()])).await?;
    row.map(|r| Ok((r.try_get("", "id")?, token))).transpose()
}

pub async fn run_one(
    db: &DatabaseConnection,
    ops: &DatabaseConnection,
    tenant: Uuid,
) -> KabiPayResult<bool> {
    let Some((id, token)) = claim(db, tenant).await? else {
        return Ok(false);
    };
    match execute_claim(db, ops, tenant, id, token).await {
        Ok(()) => {}
        Err(error) => {
            let (terminal, code) = classify(&error);
            // Business transaction has rolled back before this operational update.
            // A stale worker cannot replace a newer lease or terminal outcome.
            db.execute(statement("UPDATE organization_payroll_job SET state=CASE WHEN $4 OR attempts>=$5 THEN 'FAILED' ELSE 'QUEUED' END,failure_code=CASE WHEN attempts>=$5 THEN 'RETRY_EXHAUSTED' ELSE $6 END,finished_at=CASE WHEN $4 OR attempts>=$5 THEN clock_timestamp() ELSE NULL END,lease_token=NULL,lease_expires_at=NULL,next_attempt_at=clock_timestamp()+interval '30 seconds' WHERE tenant_id=$1 AND id=$2 AND state='RUNNING' AND lease_token=$3",vec![tenant.into(),id.into(),token.into(),terminal.into(),MAX_ATTEMPTS.into(),code.into()])).await?;
        }
    }
    Ok(true)
}
fn classify(error: &KabiPayError) -> (bool, &'static str) {
    match error {
        KabiPayError::Forbidden(_) | KabiPayError::Unauthorised => (true, "AUTHORITY_UNAVAILABLE"),
        KabiPayError::ModuleNotSubscribed(_) => (true, "MODULE_NOT_SUBSCRIBED"),
        KabiPayError::Conflict(_) | KabiPayError::ConflictRule { .. } => (true, "SOURCE_CHANGED"),
        KabiPayError::Validation(_) | KabiPayError::Json(_) | KabiPayError::BusinessRule { .. } => {
            (true, "BUSINESS_VALIDATION_FAILED")
        }
        _ => (false, "TEMPORARILY_UNAVAILABLE"),
    }
}
pub async fn execute_claim(
    db: &DatabaseConnection,
    ops: &DatabaseConnection,
    tenant: Uuid,
    id: Uuid,
    token: Uuid,
) -> KabiPayResult<()> {
    let tx = db.begin().await?;
    let result=async {
        let row=tx.query_one(statement("SELECT request_body,request_hash,authority_reference,attempts FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2 AND state='RUNNING' AND lease_token=$3 AND lease_expires_at>clock_timestamp() FOR UPDATE",vec![tenant.into(),id.into(),token.into()])).await?.ok_or_else(||KabiPayError::Internal("Payroll job claim expired".into()))?;
        if row.try_get::<i32>("","attempts")?>MAX_ATTEMPTS {return Err(KabiPayError::Validation("Payroll job retry limit reached".into()));}
        let body:Vec<u8>=row.try_get("","request_body")?;
        let command=kabipay_organization_tenant::parse_command(&body)?;
        let reference:RuntimeAuthorization=serde_json::from_value(row.try_get::<serde_json::Value>("","authority_reference")?)?;
        use sha2::{Digest,Sha256};
        if command.command_id!=id || hex::encode(Sha256::digest(&body))!=row.try_get::<String>("","request_hash")? {return Err(unavailable());}
        // Match local payroll lock ordering; hold authority and input fences until
        // business writes and terminal job state commit together.
        crate::services::loan_recovery::lock(&tx,tenant).await?;
        crate::services::payroll_fingerprint::lock_inputs(&tx).await?;
        let p=kabipay_organization_tenant::refresh_payroll_job_authority(&tx,ops,tenant,&body,&reference).await?;
        if tx.query_one(statement("SELECT id FROM payroll_cycle WHERE tenant_id=$1 AND id=$2 FOR UPDATE",vec![tenant.into(),command.resource_id.into()])).await?.is_none() {return Err(unavailable());}
        let version=tx.query_one(statement("SELECT version FROM organization_resource_version WHERE tenant_id=$1 AND resource_type='payroll_run' AND resource_id=$2 FOR UPDATE",vec![tenant.into(),command.resource_id.into()])).await?.ok_or_else(unavailable)?.try_get::<i64>("","version")?;
        if version!=command.expected_version {return Err(KabiPayError::Conflict("Source payroll changed since acceptance".into()));}
        let actor=PayrollActor::organization(&p.payroll_authority()?);
        let outcome=match command.operation {
            OrganizationOperation::PayrollDraft=>{
                let input:DraftInput=serde_json::from_value(command.payload.clone())?;
                let draft=payroll_draft::calculate_in_transaction(&tx,tenant,&actor,command.resource_id,input.expected_revision).await?;
                serde_json::json!({"cycle_id":draft.cycle_id,"revision":draft.revision,"fingerprint":draft.fingerprint,"can_finalize":draft.can_finalize,"finalization_block_reason":draft.finalization_block_reason,"employee_count":draft.employees.len()})
            },
            OrganizationOperation::PayrollFinalize=>{
                let input:FinalizeInput=serde_json::from_value(command.payload.clone())?;
                serde_json::to_value(payroll_finalize::finalize_in_transaction(&tx,tenant,&actor,command.resource_id,input.revision,&input.fingerprint,input.acknowledgement).await?)?
            },
            _=>return Err(unavailable()),
        };
        // Authority refresh and payload evaluation have bounded network calls;
        // reject a lease lost during calculation before any business commit.
        if tx.execute(statement("UPDATE organization_payroll_job SET state='SUCCEEDED',result=$4,failure_code=NULL,finished_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND id=$2 AND state='RUNNING' AND lease_token=$3 AND lease_expires_at>clock_timestamp()",vec![tenant.into(),id.into(),token.into(),outcome.into()])).await?.rows_affected()!=1 {return Err(KabiPayError::Internal("Payroll job lease expired before commit".into()));}
        Ok(())
    }.await;
    match result {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoked_authority_and_stale_source_fail_closed_but_outages_retry() {
        assert_eq!(
            classify(&KabiPayError::BusinessRule {
                code: "DOMAIN_REJECTED",
                message: "rejected".into()
            }),
            (true, "BUSINESS_VALIDATION_FAILED")
        );
        assert_eq!(classify(&unavailable()), (true, "AUTHORITY_UNAVAILABLE"));
        assert_eq!(
            classify(&KabiPayError::Conflict("changed".into())),
            (true, "SOURCE_CHANGED")
        );
        assert_eq!(
            classify(&KabiPayError::Internal("network".into())),
            (false, "TEMPORARILY_UNAVAILABLE")
        );
        assert_eq!(
            classify(&KabiPayError::ModuleNotSubscribed("PAYROLL".into())),
            (true, "MODULE_NOT_SUBSCRIBED")
        );
    }
}
