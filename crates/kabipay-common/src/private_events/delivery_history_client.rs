//! Source scoped delivery history and source authorized broker replay.
use super::{
    configured_company_keyring,
    crypto::{source_hash, Envelope, SourceHead},
    history_client::{binding_for_path, history_http_client, sign, verify, Proof},
    AuditRecordInput, DeliveryIntentInput,
};
use crate::{KabiPayError, KabiPayResult};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read, time::Duration};
use uuid::Uuid;
const BUDGET: usize = 64 * 1024 * 1024;
fn unavailable() -> KabiPayError {
    KabiPayError::Internal("private delivery history unavailable or incomplete".into())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryHistoryRecord {
    pub record_id: Uuid,
    pub resource_id: Uuid,
    pub source_kind: String,
    pub occurred_at: DateTime<Utc>,
    pub source_sha256: String,
    pub raw_source: String,
    #[serde(default)]
    pub event_name: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct DeliveryHistoryFilter {
    pub mode: String,
    pub aggregates: Vec<String>,
    pub event_ids: Vec<Uuid>,
    pub limit: u64,
    pub before: Option<DeliveryHistoryCursor>,
    #[serde(skip)]
    pub scan_all: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryHistoryCursor {
    pub occurred_at: DateTime<Utc>,
    pub event_id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
struct Coverage {
    table: String,
    id: Uuid,
    sha256: String,
}
#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    tenant_id: Uuid,
    source_generation: i64,
    request_id: Uuid,
    issued_at: i64,
    filter: &'a DeliveryHistoryFilter,
    coverages: Vec<Coverage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    tenant_id: Uuid,
    source_generation: i64,
    request_id: Uuid,
    issued_at: i64,
    request_hash: String,
    page: Page,
}
#[derive(Deserialize)]
struct Page {
    records: Vec<DeliveryHistoryRecord>,
    next_cursor: Option<DeliveryHistoryCursor>,
}
pub fn decode_record(r: &DeliveryHistoryRecord, tenant: Uuid) -> KabiPayResult<Value> {
    if source_hash(r.raw_source.as_bytes()) != r.source_sha256 {
        return Err(unavailable());
    }
    let v: Value = serde_json::from_str(&r.raw_source).map_err(|_| unavailable())?;
    if v.get("id").and_then(Value::as_str) != Some(r.record_id.to_string().as_str())
        || v.get("tenant_id").and_then(Value::as_str) != Some(tenant.to_string().as_str())
    {
        return Err(unavailable());
    }
    match r.source_kind.as_str() {
        "webhook" => {
            let intent: DeliveryIntentInput =
                serde_json::from_str(&r.raw_source).map_err(|_| unavailable())?;
            if intent.resource_id != r.resource_id
                || intent.source_kind != "webhook"
                || intent.occurred_at.timestamp_micros() != r.occurred_at.timestamp_micros()
            {
                return Err(unavailable());
            }
        }
        "legacy_outbox_event" | "legacy_webhook_delivery_log" => {
            if r.resource_id != r.record_id {
                return Err(unavailable());
            }
        }
        "delivery_outcome" => {
            if v.get("intent_id").and_then(Value::as_str)
                != Some(r.resource_id.to_string().as_str())
            {
                return Err(unavailable());
            }
        }
        _ => return Err(unavailable()),
    }
    Ok(v)
}
fn insert(
    records: &mut BTreeMap<Uuid, DeliveryHistoryRecord>,
    r: DeliveryHistoryRecord,
    tenant: Uuid,
) -> KabiPayResult<()> {
    decode_record(&r, tenant)?;
    if let Some(prior) = records.get(&r.record_id) {
        if prior.source_sha256 != r.source_sha256
            || prior.source_kind != r.source_kind
            || prior.resource_id != r.resource_id
            || prior.occurred_at != r.occurred_at
        {
            return Err(KabiPayError::Conflict(
                "delivery history identity conflict".into(),
            ));
        }
    } else {
        records.insert(r.record_id, r);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_raw_rows_bind_hash_tenant_resource_and_large_integers() {
        let tenant = Uuid::new_v4();
        let intent = DeliveryIntentInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            destination_id: Uuid::new_v4(),
            destination_version: 1,
            source_kind: "webhook".into(),
            resource_id: Uuid::new_v4(),
            event_type: "expense.approved".into(),
            payload: json!({"n":u64::MAX}),
            occurred_at: Utc::now(),
        };
        let raw = serde_json::to_string(&intent).unwrap();
        let mut row = DeliveryHistoryRecord {
            record_id: intent.id,
            resource_id: intent.resource_id,
            source_kind: "webhook".into(),
            occurred_at: intent.occurred_at,
            source_sha256: source_hash(raw.as_bytes()),
            raw_source: raw,
            event_name: None,
        };
        assert_eq!(
            decode_record(&row, tenant).unwrap()["payload"]["n"].as_u64(),
            Some(u64::MAX)
        );
        assert!(decode_record(&row, Uuid::new_v4()).is_err());
        row.resource_id = Uuid::new_v4();
        assert!(decode_record(&row, tenant).is_err());
        row.resource_id = intent.resource_id;
        row.raw_source.push(' ');
        assert!(decode_record(&row, tenant).is_err());
    }
    #[tokio::test]
    #[ignore = "requires explicit isolated PRIVATE_EVENT_TEST_DATABASE_URL"]
    async fn postgres_replay_restores_exact_intent_audits_and_preserves_terminal_fences() {
        use sea_orm::Database;
        let db = Database::connect(
            std::env::var("PRIVATE_EVENT_TEST_DATABASE_URL").expect("isolated DB URL required"),
        )
        .await
        .unwrap();
        let tx = db.begin().await.unwrap();
        for sql in [
            "CREATE TEMP TABLE \"user\"(id uuid PRIMARY KEY,tenant_id uuid NOT NULL) ON COMMIT DROP",
            "CREATE TEMP TABLE private_source_cutover(tenant_id uuid PRIMARY KEY,producer_generation bigint NOT NULL) ON COMMIT DROP",
            "CREATE TEMP TABLE private_delivery_receipt(tenant_id uuid,intent_id uuid,destination_id uuid,source_kind text,resource_id uuid,intent_hash char(64),destination_version bigint,source_generation bigint,status text,lease_generation bigint,lease_until timestamptz,claim_owner uuid,outcome_code text,updated_at timestamptz,PRIMARY KEY(tenant_id,intent_id,destination_id)) ON COMMIT DROP",
            "CREATE TEMP TABLE company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid,organization_id uuid,event_type text,aggregate_type text,aggregate_id uuid,aggregate_version bigint,event_envelope jsonb NOT NULL,event_stream text,created_at timestamptz) ON COMMIT DROP",
        ]{tx.execute_unprepared(sql).await.unwrap();}
        let tenant = Uuid::new_v4();
        let actor = Uuid::new_v4();
        let intent = DeliveryIntentInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            destination_id: Uuid::new_v4(),
            destination_version: 2,
            source_kind: "webhook".into(),
            resource_id: Uuid::new_v4(),
            event_type: "expense.approved".into(),
            payload: json!({"body":"original"}),
            occurred_at: Utc::now(),
        };
        let keys=super::super::crypto::Keyring::from_json(&serde_json::to_vec(&json!({"schema_version":1,"keys":[{"tenant_id":tenant,"purpose":"audit","key_id":"a1","key_b64":"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=","active":true},{"tenant_id":tenant,"purpose":"delivery","key_id":"d1","key_b64":"CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg=","active":true}]})).unwrap()).unwrap();
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO private_source_cutover VALUES($1,17)",
            [tenant.into()],
        ))
        .await
        .unwrap();
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO \"user\" VALUES($1,$2)",
            [actor.into(), tenant.into()],
        ))
        .await
        .unwrap();
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_delivery_receipt(tenant_id,intent_id,destination_id,source_kind,resource_id,intent_hash,destination_version,source_generation,status,lease_generation) VALUES($1,$2,$3,'webhook',$4,$5,2,17,'PERMANENT',7)",[tenant.into(),intent.id.into(),intent.destination_id.into(),intent.resource_id.into(),source_hash(&serde_json::to_vec(&intent).unwrap()).into()])).await.unwrap();
        for state in [
            "DELIVERED",
            "UNCERTAIN",
            "PROCESSING",
            "PENDING",
            "DISABLED",
            "NO_DESTINATION",
        ] {
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE private_delivery_receipt SET status=$1",
                [state.into()],
            ))
            .await
            .unwrap();
            assert!(
                append_delivery_replay_with_keyring(&tx, tenant, actor, &intent, &keys)
                    .await
                    .is_err()
            );
        }
        tx.execute_unprepared("UPDATE private_delivery_receipt SET status='PERMANENT'")
            .await
            .unwrap();
        let savepoint = tx.begin().await.unwrap();
        assert!(append_delivery_replay_with_keyring(
            &savepoint,
            tenant,
            Uuid::new_v4(),
            &intent,
            &keys
        )
        .await
        .is_err());
        savepoint.rollback().await.unwrap();
        let count = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS n FROM company_event_outbox",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(count, 0);
        append_delivery_replay_with_keyring(&tx, tenant, actor, &intent, &keys)
            .await
            .unwrap();
        let receipt = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT status,lease_generation FROM private_delivery_receipt",
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.try_get::<String>("", "status").unwrap(), "PENDING");
        assert_eq!(receipt.try_get::<i64>("", "lease_generation").unwrap(), 7);
        let rows=tx.query_all(Statement::from_string(DbBackend::Postgres,"SELECT event_envelope FROM company_event_outbox WHERE event_envelope->>'source_kind'='delivery_replay'")).await.unwrap();
        assert_eq!(rows.len(), 1);
        let frame: Envelope =
            serde_json::from_value(rows[0].try_get::<Value>("", "event_envelope").unwrap())
                .unwrap();
        let command: DeliveryIntentInput =
            serde_json::from_slice(&keys.decrypt_frames(&[frame], tenant, "delivery").unwrap())
                .unwrap();
        assert_eq!(command.resource_id, intent.id);
        assert_ne!(command.id, intent.id);
        assert_eq!(command.payload["base_lease"], 7);
        assert_eq!(command.payload["retry_budget"], 3);
        assert!(
            append_delivery_replay_with_keyring(&tx, tenant, actor, &intent, &keys)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
}
pub async fn read_configured_delivery_history<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    tenant: Uuid,
    filter: &DeliveryHistoryFilter,
) -> KabiPayResult<Vec<DeliveryHistoryRecord>> {
    if tenant.is_nil()
        || !matches!(filter.mode.as_str(), "events" | "logs")
        || filter.limit == 0
        || filter.limit > 500
        || filter.event_ids.len() > 500
        || filter.aggregates.len() > 16
    {
        return Err(KabiPayError::Validation(
            "invalid delivery history filter".into(),
        ));
    }
    tokio::time::timeout(Duration::from_secs(20), read_inner(db, tenant, filter))
        .await
        .map_err(|_| unavailable())?
}
async fn read_inner<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    tenant: Uuid,
    filter: &DeliveryHistoryFilter,
) -> KabiPayResult<Vec<DeliveryHistoryRecord>> {
    let path =
        crate::db::company_setting(tenant, "KABIPAY_PRIVATE_DELIVERY_HISTORY_CLIENT_FILE")?;
    let mut config = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(65537).read_to_end(&mut config))
        .map_err(|_| unavailable())?;
    let (url, key) =
        binding_for_path(&config, tenant, "/internal/private-events/delivery-history")?;
    let tx = db.begin().await?;
    tx.execute_unprepared("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let rows=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT DISTINCT ON(e.source_table) a.producer_generation,e.source_table,e.id,e.source_sha256 FROM private_source_cutover a JOIN private_legacy_export e ON e.tenant_id=a.tenant_id AND e.producer_generation<=a.producer_generation AND e.source_version=a.source_version WHERE a.tenant_id=$1 AND e.source_table IN ('outbox_event','webhook_delivery_log','employee_finance_outbox') AND e.source_database=current_database() AND e.source_schema=current_schema() AND e.accepted_at<=clock_timestamp() AND e.reader_switch_complete AND e.revoked_at IS NULL AND e.archive_row_count=e.row_count AND e.archive_sha256=e.source_sha256 AND e.restore_sha256=e.source_sha256 AND e.restore_verified_at<=e.accepted_at ORDER BY e.source_table,e.producer_generation DESC,e.accepted_at DESC,e.id DESC",[tenant.into()])).await?;
    if rows.len() != 3 {
        return Err(unavailable());
    }
    let generation: i64 = rows[0].try_get("", "producer_generation")?;
    let mut coverages = Vec::new();
    for row in rows {
        coverages.push(Coverage {
            table: row.try_get("", "source_table")?,
            id: row.try_get("", "id")?,
            sha256: row.try_get("", "source_sha256")?,
        });
    }
    let keys = configured_company_keyring(tenant)?;
    let mut frames: BTreeMap<Uuid, Vec<Envelope>> = BTreeMap::new();
    let mut cursor = Uuid::nil();
    let mut size = 0usize;
    loop {
        let rows=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,event_envelope FROM company_event_outbox WHERE source_tenant_id=$1 AND event_stream='delivery' AND event_envelope->>'source_kind' IN ('webhook','legacy_outbox_event','legacy_webhook_delivery_log','delivery_outcome') AND id>$2 ORDER BY id LIMIT 256",[tenant.into(),cursor.into()])).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            cursor = row.try_get("", "id")?;
            let value: Value = row.try_get("", "event_envelope")?;
            size = size
                .checked_add(serde_json::to_vec(&value)?.len())
                .ok_or_else(unavailable)?;
            if size > BUDGET {
                return Err(unavailable());
            }
            let frame: Envelope = serde_json::from_value(value).map_err(|_| unavailable())?;
            if frame.event_id != cursor
                || frame.tenant_id != tenant
                || frame.purpose != "delivery"
                || frame.source_generation > generation
            {
                return Err(unavailable());
            }
            frames.entry(frame.record_id).or_default().push(frame);
        }
    }
    let mut records = BTreeMap::new();
    for (id, frames) in frames {
        let raw = keys.decrypt_frames(&frames, tenant, "delivery")?;
        let head = &frames[0];
        let r = DeliveryHistoryRecord {
            record_id: id,
            resource_id: head.resource_id,
            source_kind: head.source_kind.clone(),
            occurred_at: DateTime::parse_from_rfc3339(&head.occurred_at)
                .map_err(|_| unavailable())?
                .with_timezone(&Utc),
            source_sha256: head.source_sha256.clone(),
            raw_source: String::from_utf8(raw).map_err(|_| unavailable())?,
            event_name: None,
        };
        insert(&mut records, r, tenant)?;
    }
    tx.commit().await?;
    let mut remote_filter = filter.clone();
    loop {
        let request = Request {
            version: 1,
            tenant_id: tenant,
            source_generation: generation,
            request_id: Uuid::new_v4(),
            issued_at: Utc::now().timestamp(),
            filter: &remote_filter,
            coverages: coverages.clone(),
        };
        let raw = serde_json::to_vec(&request)?;
        let proof = sign(&raw, &key, "kabipay-private-delivery-history-request-v1")?;
        let mut response = history_http_client(tenant)?
            .post(url.clone())
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&proof)?)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > (BUDGET * 4 / 3 + 1024) as u64)
        {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > BUDGET * 4 / 3 + 1024 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let reply: Proof = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        let reply: Response = serde_json::from_slice(&verify(
            &reply,
            &key,
            "kabipay-private-delivery-history-response-v1",
        )?)
        .map_err(|_| unavailable())?;
        if reply.version != 1
            || reply.tenant_id != tenant
            || reply.source_generation != generation
            || reply.request_id != request.request_id
            || reply.request_hash != source_hash(&raw)
            || reply.issued_at < Utc::now().timestamp() - 60
            || reply.issued_at > Utc::now().timestamp() + 5
            || reply.page.records.len() > filter.limit as usize * 258
        {
            return Err(unavailable());
        }
        let next_cursor = reply.page.next_cursor;
        for row in reply.page.records {
            size = size
                .checked_add(row.raw_source.len())
                .ok_or_else(unavailable)?;
            if size > BUDGET {
                return Err(unavailable());
            }
            insert(&mut records, row, tenant)?;
        }
        if !filter.scan_all {
            break;
        }
        match next_cursor {
            Some(cursor) => {
                if remote_filter.before.as_ref().is_some_and(|prior| {
                    (cursor.occurred_at, cursor.event_id) >= (prior.occurred_at, prior.event_id)
                }) {
                    return Err(unavailable());
                }
                remote_filter.before = Some(cursor);
            }
            None => break,
        }
    }
    Ok(records.into_values().collect())
}
/// Caller owns the transaction containing receipt authorization, audit and broker command.
pub async fn append_delivery_replay<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    intent: &DeliveryIntentInput,
) -> KabiPayResult<()> {
    append_delivery_replay_with_keyring(db, tenant, actor, intent, &configured_company_keyring(tenant)?).await
}
async fn append_delivery_replay_with_keyring<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    intent: &DeliveryIntentInput,
    keys: &super::crypto::Keyring,
) -> KabiPayResult<()> {
    db.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1 FOR SHARE",
        [tenant.into()],
    ))
    .await?
    .ok_or_else(unavailable)?;
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT r.*,a.producer_generation FROM private_source_cutover a JOIN private_delivery_receipt r ON r.tenant_id=a.tenant_id AND r.source_generation=a.producer_generation WHERE r.tenant_id=$1 AND r.intent_id=$2 AND r.destination_id=$3 FOR UPDATE OF r FOR SHARE OF a",[tenant.into(),intent.id.into(),intent.destination_id.into()])).await?.ok_or_else(unavailable)?;
    let status: String = row.try_get("", "status")?;
    if !matches!(status.as_str(), "RETRYABLE" | "PERMANENT") {
        return Err(KabiPayError::Conflict(
            "only failed destinations may be replayed; UNCERTAIN requires explicit reconciliation"
                .into(),
        ));
    }
    let generation: i64 = row.try_get("", "producer_generation")?;
    let base_lease: i64 = row.try_get("", "lease_generation")?;
    let hash: String = row.try_get("", "intent_hash")?;
    if intent.tenant_id != tenant
        || intent.source_kind != "webhook"
        || hash != source_hash(&serde_json::to_vec(intent)?)
        || row.try_get::<i64>("", "destination_version")? != intent.destination_version
        || row.try_get::<Uuid>("", "resource_id")? != intent.resource_id
        || base_lease > 9_007_199_254_740_987
    {
        return Err(unavailable());
    }
    // A terminal receipt may outlive its source payload. Restore only the exact
    // authenticated immutable intent before reauthorizing it; the broker never
    // supplies replacement payloads, endpoints or credentials.
    // The receipt UPDATE lock above fences cleanup, whose candidate selection and
    // delete guard both take receipt SHARE locks before deleting private frames.
    // Frames are immutable. If cleanup committed first, the exact intent is
    // restored below; locking the outbox would require forbidden UPDATE rights.
    let source_rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT event_envelope FROM company_event_outbox WHERE source_tenant_id=$1 AND event_stream='delivery' AND event_envelope->>'record_id'=$2 ORDER BY id LIMIT 65",[tenant.into(),intent.id.to_string().into()])).await?;
    if source_rows.is_empty() {
        let frames = keys.encrypt_frames(
            &serde_json::to_vec(intent)?,
            &SourceHead {
                record_id: intent.id,
                tenant_id: tenant,
                purpose: "delivery".into(),
                source_generation: generation,
                source_kind: "webhook".into(),
                resource_type: "delivery_intent".into(),
                resource_id: intent.resource_id,
                occurred_at: intent
                    .occurred_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            },
        )?;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            super::INSERT_FRAMES,
            [serde_json::to_value(frames)?.into()],
        ))
        .await?;
    } else {
        let frames: Vec<Envelope> = source_rows
            .into_iter()
            .map(|r| {
                r.try_get::<Value>("", "event_envelope")
                    .map_err(KabiPayError::from)
                    .and_then(|v| serde_json::from_value(v).map_err(|_| unavailable()))
            })
            .collect::<KabiPayResult<_>>()?;
        if source_hash(&keys.decrypt_frames(&frames, tenant, "delivery")?) != hash {
            return Err(unavailable());
        }
    }
    let command = DeliveryIntentInput {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        destination_id: intent.destination_id,
        destination_version: intent.destination_version,
        source_kind: "delivery_replay".into(),
        resource_id: intent.id,
        event_type: "private.delivery.replay.v1".into(),
        payload: json!({"intent_id":intent.id,"intent_hash":hash,"destination_id":intent.destination_id,"destination_version":intent.destination_version,"source_generation":generation,"base_lease":base_lease,"retry_budget":3}),
        occurred_at: super::canonical_private_time(Utc::now())?,
    };
    super::append_audit_with_keyring(db,&AuditRecordInput{id:Uuid::new_v4(),tenant_id:tenant,user_id:Some(actor),entity_type:"DELIVERY".into(),entity_id:Some(intent.resource_id),action:"DELIVERY_REPLAY_AUTHORIZED".into(),before_state:Some(json!({"status":status,"lease_generation":base_lease})),after_state:Some(json!({"command_id":command.id,"intent_id":intent.id,"destination_id":intent.destination_id})),ip_address:None,user_agent:None,created_at:command.occurred_at},keys).await?;
    let frames = keys.encrypt_frames(
        &serde_json::to_vec(&command)?,
        &SourceHead {
            record_id: command.id,
            tenant_id: tenant,
            purpose: "delivery".into(),
            source_generation: generation,
            source_kind: "delivery_replay".into(),
            resource_type: "delivery_replay".into(),
            resource_id: intent.id,
            occurred_at: command
                .occurred_at
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        },
    )?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        super::INSERT_FRAMES,
        [serde_json::to_value(frames)?.into()],
    ))
    .await?;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET status='PENDING',lease_until=NULL,claim_owner=NULL,outcome_code='manual_replay_authorized',updated_at=clock_timestamp() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND lease_generation=$4 AND status IN ('RETRYABLE','PERMANENT')",[tenant.into(),intent.id.into(),intent.destination_id.into(),base_lease.into()])).await?;
    Ok(())
}
