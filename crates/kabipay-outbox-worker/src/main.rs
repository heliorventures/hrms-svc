//! Polls tenant `outbox_event` rows (`PENDING`), claims with `FOR UPDATE SKIP LOCKED`,
//! delivers to each active `webhook_subscription` (event filter or `*`), then optional
//! global `OUTBOX_WEBHOOK_URL`, then marks `PROCESSED` or `FAILED` / requeues with backoff.

use anyhow::Context;
use chrono::{Duration as ChronoDuration, Utc};
use hmac::{Hmac, Mac};
use kabipay_common::db::{connect_ops_db, resolve_tenant_db, TenantDbCache, TenantDbConfig};
use kabipay_common::load_dotenv;
use kabipay_common::private_file_cleanup::{
    process_private_file_cleanup_tasks, sweep_expired_company_upload_stages,
};
use kabipay_common::subgraph::{ops_dsn_from_env, tenant_db_config_from_env};
use kabipay_common::telemetry::init_tracing;
use kabipay_db_entities::ops::tenant_database;
use kabipay_db_entities::tenant::d0026_integrations::{webhook_delivery_log, webhook_subscription};
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, Set, Statement, TransactionTrait,
};
// `sea_orm::FromQueryResult` at crate root is the derive macro (shadows the trait). Import the trait explicitly for `find_by_statement`.
use sea_orm::entity::FromQueryResult;
use serde_json::json;
use sha2::Sha256;
use std::collections::HashSet;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const STATUS_PENDING: &str = "PENDING";
const STATUS_PROCESSING: &str = "PROCESSING";
const STATUS_PROCESSED: &str = "PROCESSED";
const STATUS_FAILED: &str = "FAILED";

#[derive(Debug, sea_orm::FromQueryResult)]
struct PickId {
    id: Uuid,
}

fn poll_interval() -> Duration {
    let ms: u64 = std::env::var("OUTBOX_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    Duration::from_millis(ms.max(250))
}

/// PROCESSING rows with `claimed_at` older than this are reset to PENDING (worker crash mid-flight).
fn stale_processing_duration() -> std::time::Duration {
    let sec: u64 = std::env::var("OUTBOX_STALE_PROCESSING_SEC")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
        .max(30);
    std::time::Duration::from_secs(sec)
}

/// `PROCESSING` rows with NULL `claimed_at` (pre-0034) are reclaimed if older than this from `created_at`.
fn legacy_null_claimed_max_age() -> std::time::Duration {
    let sec: u64 = std::env::var("OUTBOX_LEGACY_PROCESSING_MAX_AGE_SEC")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
        .max(300);
    std::time::Duration::from_secs(sec)
}

fn max_retries() -> i32 {
    std::env::var("OUTBOX_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .max(1)
}

fn webhook_url() -> Option<String> {
    let v = std::env::var("OUTBOX_WEBHOOK_URL").ok()?;
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn skip_tenant_webhook_subscriptions() -> bool {
    matches!(
        std::env::var("OUTBOX_SKIP_TENANT_WEBHOOKS")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes"
    )
}

/// Optional **platform** signing key for outbound webhook `POST` bodies. Receivers verify
/// `X-KabiPay-Signature` = `v1=` + hex(HMAC-SHA256(key, "{unix_ts}.{body_utf8}")) with `X-KabiPay-Timestamp`.
/// Per-tenant secrets from **Insights → Register webhook** are still stored as **SHA-256 only**; use this
/// env for integrators who share one verification key (or until encrypted per-subscription key storage ships).
fn outbound_webhook_signing_key() -> Option<Vec<u8>> {
    if let Ok(h) = std::env::var("OUTBOX_WEBHOOK_SIGNING_SECRET_HEX") {
        let t = h.trim();
        if t.is_empty() {
            return None;
        }
        return hex::decode(t).ok();
    }
    let raw = std::env::var("OUTBOX_WEBHOOK_SIGNING_SECRET").ok()?;
    let t = raw.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.as_bytes().to_vec())
    }
}

fn webhook_hmac_v1_hex(key: &[u8], unix_ts: i64, body_utf8: &str) -> anyhow::Result<String> {
    let msg = format!("{unix_ts}.{body_utf8}");
    let mut mac = HmacSha256::new_from_slice(key).context("HMAC key length")?;
    mac.update(msg.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn signing_headers_for_body(key: &[u8], body_utf8: &str) -> anyhow::Result<HeaderMap> {
    let ts = Utc::now().timestamp();
    let sig = webhook_hmac_v1_hex(key, ts, body_utf8)?;
    let mut h = HeaderMap::new();
    h.insert(
        "X-KabiPay-Timestamp",
        HeaderValue::from_str(&ts.to_string()).context("timestamp header")?,
    );
    h.insert(
        "X-KabiPay-Signature",
        HeaderValue::from_str(&format!("v1={sig}")).context("signature header")?,
    );
    Ok(h)
}

async fn post_json_with_hmac(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> anyhow::Result<(u16, String)> {
    let body_vec = serde_json::to_vec(body).context("serialize webhook JSON body")?;
    let body_utf8 = std::str::from_utf8(&body_vec).context("webhook JSON must be UTF-8")?;
    let signing = outbound_webhook_signing_key()
        .map(|k| signing_headers_for_body(&k, body_utf8))
        .transpose()
        .context("HMAC signing headers")?;

    let mut req = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .body(body_vec);
    if let Some(headers) = signing {
        req = req.headers(headers);
    }

    let res = req.send().await.with_context(|| format!("webhook POST {url}"))?;
    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    Ok((status, text))
}

fn subscription_matches_event(filter: &str, event_type: &str) -> bool {
    let f = filter.trim();
    f == "*" || f.eq_ignore_ascii_case(event_type)
}

fn build_delivery_body(model: &outbox_event::Model) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "outbox_id": model.id,
        "tenant_id": model.tenant_id,
        "aggregate_type": model.aggregate_type,
        "aggregate_id": model.aggregate_id,
        "event_type": model.event_type,
        "payload": model.payload,
        "created_at": model.created_at,
    })
}

async fn record_webhook_delivery(
    tenant_db: &DatabaseConnection,
    tenant_id: Uuid,
    subscription_id: Uuid,
    event_type: &str,
    payload: &serde_json::Value,
    http_status: Option<i32>,
    response_body: &str,
    is_success: bool,
    attempt_no: i32,
) -> anyhow::Result<()> {
    let log_id = Uuid::new_v4();
    let now = Utc::now();
    let mut body_snippet = response_body.to_string();
    if body_snippet.len() > 4000 {
        body_snippet = format!("{}…", &body_snippet[..3997]);
    }
    let am = webhook_delivery_log::ActiveModel {
        id: Set(log_id),
        tenant_id: Set(tenant_id),
        webhook_subscription_id: Set(subscription_id),
        event_name: Set(Some(event_type.to_string())),
        payload_json: Set(Some(payload.clone().into())),
        http_status: Set(http_status),
        response_body: Set(Some(body_snippet)),
        is_success: Set(is_success),
        attempt_number: Set(attempt_no),
        delivered_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(tenant_db)
        .await
        .with_context(|| format!("insert webhook_delivery_log {}", log_id))?;
    Ok(())
}

async fn active_tenant_ids(ops: &DatabaseConnection) -> anyhow::Result<Vec<Uuid>> {
    let rows = tenant_database::Entity::find()
        .filter(tenant_database::Column::IsActive.eq(true))
        .all(ops)
        .await
        .context("list tenant_database")?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in rows {
        if seen.insert(r.tenant_id) {
            out.push(r.tenant_id);
        }
    }
    Ok(out)
}

async fn claim_next_pending(
    tenant_db: &DatabaseConnection,
    tenant_id: Uuid,
) -> anyhow::Result<Option<outbox_event::Model>> {
    let txn = tenant_db.begin().await.context("begin claim txn")?;
    let pick_stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"SELECT id FROM outbox_event
           WHERE tenant_id = $1 AND status = $2
           ORDER BY created_at ASC
           FOR UPDATE SKIP LOCKED
           LIMIT 1"#,
        vec![tenant_id.into(), STATUS_PENDING.into()],
    );
    let picked = PickId::find_by_statement(pick_stmt)
        .one(&txn)
        .await
        .context("pick pending outbox row")?;
    let Some(picked) = picked else {
        txn.commit().await?;
        return Ok(None);
    };

    let mut am: outbox_event::ActiveModel = outbox_event::Entity::find_by_id(picked.id)
        .one(&txn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("outbox row vanished after lock"))?
        .into();
    am.status = Set(STATUS_PROCESSING.into());
    am.claimed_at = Set(Some(Utc::now()));
    let updated = am.update(&txn).await.context("set PROCESSING")?;
    txn.commit().await?;
    Ok(Some(updated))
}

async fn mark_processed(tenant_db: &DatabaseConnection, id: Uuid) -> anyhow::Result<()> {
    let Some(m) = outbox_event::Entity::find_by_id(id)
        .one(tenant_db)
        .await
        .context("load outbox for PROCESSED")?
    else {
        return Ok(());
    };
    if m.status != STATUS_PROCESSING {
        return Ok(());
    }
    let mut am: outbox_event::ActiveModel = m.into();
    am.status = Set(STATUS_PROCESSED.into());
    am.processed_at = Set(Some(Utc::now()));
    am.claimed_at = Set(None);
    am.last_error = Set(None);
    am.update(tenant_db).await.context("mark PROCESSED")?;
    Ok(())
}

async fn mark_failure(
    tenant_db: &DatabaseConnection,
    id: Uuid,
    prev_retry: i32,
    err: &str,
    cap: i32,
) -> anyhow::Result<()> {
    let Some(m) = outbox_event::Entity::find_by_id(id)
        .one(tenant_db)
        .await
        .context("load outbox for failure")?
    else {
        return Ok(());
    };
    if m.status != STATUS_PROCESSING {
        return Ok(());
    }
    let next_retry = prev_retry + 1;
    let (status, processed_at) = if next_retry >= cap {
        (STATUS_FAILED, Some(Utc::now()))
    } else {
        (STATUS_PENDING, None)
    };
    let truncated = if err.len() > 2000 {
        format!("{}…", &err[..1997])
    } else {
        err.to_string()
    };
    let mut am: outbox_event::ActiveModel = m.into();
    am.status = Set(status.into());
    am.retry_count = Set(next_retry);
    am.last_error = Set(Some(truncated));
    am.processed_at = Set(processed_at);
    if status == STATUS_PENDING || status == STATUS_FAILED {
        am.claimed_at = Set(None);
    }
    am.update(tenant_db).await.context("mark failure / retry")?;
    Ok(())
}

async fn deliver_event(tenant_db: &DatabaseConnection, model: &outbox_event::Model) -> anyhow::Result<()> {
    tracing::info!(
        tenant_id = %model.tenant_id,
        outbox_id = %model.id,
        aggregate_type = %model.aggregate_type,
        aggregate_id = %model.aggregate_id,
        event_type = %model.event_type,
        retry_count = model.retry_count,
        "outbox deliver"
    );
    let body = build_delivery_body(model);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("reqwest client")?;
    let attempt_no = model.retry_count + 1;

    // Tenant-configured webhook endpoints (insights → registerWebhookSubscription).
    if !skip_tenant_webhook_subscriptions() {
        let subs = webhook_subscription::Entity::find()
            .filter(webhook_subscription::Column::TenantId.eq(model.tenant_id))
            .filter(webhook_subscription::Column::IsActive.eq(true))
            .order_by_asc(webhook_subscription::Column::CreatedAt)
            .all(tenant_db)
            .await
            .with_context(|| "list webhook_subscription")?;

        let et = model.event_type.as_str();
        for sub in subs {
            if !subscription_matches_event(&sub.event_name, et) {
                continue;
            }
            let endpoint = sub.endpoint_url.trim();
            if endpoint.is_empty() {
                tracing::warn!(subscription_id = %sub.id, "webhook_subscription empty endpoint_url; skip");
                continue;
            }
            match post_json_with_hmac(&client, endpoint, &body).await {
                Ok((status, text)) => {
                    let ok = status >= 200 && status < 300;
                    if let Err(e) = record_webhook_delivery(
                        tenant_db,
                        model.tenant_id,
                        sub.id,
                        et,
                        &body,
                        Some(status as i32),
                        &text,
                        ok,
                        attempt_no,
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "webhook_delivery_log insert failed (delivery already attempted)");
                    }
                    if !ok {
                        anyhow::bail!("tenant webhook {endpoint} returned HTTP {status}: {}", text.chars().take(500).collect::<String>());
                    }
                }
                Err(e) => {
                    let _ = record_webhook_delivery(
                        tenant_db,
                        model.tenant_id,
                        sub.id,
                        et,
                        &body,
                        None,
                        &e.to_string(),
                        false,
                        attempt_no,
                    )
                    .await;
                    return Err(e);
                }
            }
        }
    }

    if let Some(url) = webhook_url() {
        let (status, text) = post_json_with_hmac(&client, &url, &body)
            .await
            .context("global OUTBOX_WEBHOOK_URL POST")?;
        let ok = status >= 200 && status < 300;
        if !ok {
            anyhow::bail!("OUTBOX_WEBHOOK_URL returned HTTP {status}: {}", text.chars().take(500).collect::<String>());
        }
    }
    Ok(())
}

/// Reset stuck PROCESSING rows (dead worker) back to PENDING for retry.
async fn reclaim_stale_processing(
    tenant_db: &DatabaseConnection,
    tenant_id: Uuid,
) -> anyhow::Result<u64> {
    let stale_line = Utc::now()
        - ChronoDuration::from_std(stale_processing_duration())
            .unwrap_or_else(|_| ChronoDuration::seconds(120));
    let legacy_line = Utc::now()
        - ChronoDuration::from_std(legacy_null_claimed_max_age())
            .unwrap_or_else(|_| ChronoDuration::hours(1));
    let res = tenant_db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE outbox_event
               SET status = $1,
                   last_error = LEFT(COALESCE(last_error, '') || ' [reclaimed stale PROCESSING]', 2000),
                   claimed_at = NULL
               WHERE tenant_id = $2
                 AND status = $3
                 AND (
                   (claimed_at IS NOT NULL AND claimed_at < $4)
                   OR (claimed_at IS NULL AND created_at < $5)
                 )"#,
            vec![
                STATUS_PENDING.into(),
                tenant_id.into(),
                STATUS_PROCESSING.into(),
                stale_line.into(),
                legacy_line.into(),
            ],
        ))
        .await
        .context("reclaim stale processing")?;
    Ok(res.rows_affected())
}

async fn process_tenant_outbox(
    tenant_db: &DatabaseConnection,
    tenant_id: Uuid,
    cap: i32,
) -> anyhow::Result<usize> {
    let r = reclaim_stale_processing(tenant_db, tenant_id).await?;
    if r > 0 {
        tracing::info!(%tenant_id, reclaimed = r, "outbox reclaimed stale PROCESSING rows");
    }
    let mut n = 0;
    while let Some(ev) = claim_next_pending(tenant_db, tenant_id).await? {
        n += 1;
        match deliver_event(tenant_db, &ev).await {
            Ok(()) => mark_processed(tenant_db, ev.id).await?,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    outbox_id = %ev.id,
                    "outbox delivery failed"
                );
                mark_failure(tenant_db, ev.id, ev.retry_count, &e.to_string(), cap).await?;
            }
        }
    }
    Ok(n)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    init_tracing("kabipay-outbox-worker");

    let dsn = ops_dsn_from_env();
    let ops_db = connect_ops_db(&dsn)
        .await
        .map_err(|e| anyhow::anyhow!("ops db {dsn}: {e}"))?;
    let cache = TenantDbCache::new();
    let fallback: TenantDbConfig = tenant_db_config_from_env();
    let cap = max_retries();
    let interval = poll_interval();

    tracing::info!(
        poll_ms = interval.as_millis(),
        max_retries = cap,
        stale_processing_sec = stale_processing_duration().as_secs(),
        legacy_processing_max_sec = legacy_null_claimed_max_age().as_secs(),
        global_webhook = webhook_url().is_some(),
        skip_tenant_webhooks = skip_tenant_webhook_subscriptions(),
        "outbox worker started"
    );

    loop {
        match active_tenant_ids(&ops_db).await {
            Ok(tenants) => {
                for tid in tenants {
                    match resolve_tenant_db(tid, &ops_db, &cache, &fallback).await {
                        Ok(tdb) => {
                            if let Err(error) = sweep_expired_company_upload_stages(&tdb, tid, 25).await {
                                tracing::error!(%tid, code = error.code(), "expired file-upload stage sweep failed");
                            }
                            if let Err(error) = process_private_file_cleanup_tasks(&tdb, tid, 25).await {
                                tracing::error!(%tid, code = error.code(), "private file cleanup sweep failed");
                            }
                            if let Err(e) = process_tenant_outbox(&tdb, tid, cap).await {
                                tracing::error!(%tid, error = %e, "tenant outbox sweep failed");
                            }
                        }
                        Err(e) => tracing::error!(%tid, error = %e, "resolve tenant db failed"),
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "list tenants failed"),
        }
        tokio::time::sleep(interval).await;
    }
}
