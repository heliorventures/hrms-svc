//! Authenticated, tenant and purpose scoped private event publication.
pub mod crypto;
pub mod delivery_history_client;
pub mod history_client;
pub mod types;
use crate::{KabiPayError, KabiPayResult};
use chrono::SecondsFormat;
use crypto::{Envelope, Keyring, SourceHead};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::io::Read;
pub use types::*;
use uuid::Uuid;

/// Load only the operator configured key file. Runtime input cannot select its path.
pub fn configured_keyring() -> KabiPayResult<Keyring> {
    if crate::db::shared_company_ids()?.is_some() {
        return Err(KabiPayError::Internal("shared private event access requires a company".into()));
    }
    let path = std::env::var_os("KABIPAY_PRIVATE_EVENT_KEYS_FILE")
        .ok_or_else(|| KabiPayError::Internal("private event keys are not configured".into()))?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(65537).read_to_end(&mut bytes))
        .map_err(|_| {
            KabiPayError::Internal("private event key configuration unavailable".into())
        })?;
    Keyring::from_json(&bytes)
}

pub fn configured_company_keyring(tenant: Uuid) -> KabiPayResult<Keyring> {
    let path = crate::db::company_setting(tenant, "KABIPAY_PRIVATE_EVENT_KEYS_FILE")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(65537).read_to_end(&mut bytes))
        .map_err(|_| KabiPayError::Internal("private event key configuration unavailable".into()))?;
    Keyring::from_json(&bytes)
}

fn audit_frames(
    input: &AuditRecordInput,
    keys: &Keyring,
    generation: i64,
) -> KabiPayResult<Vec<Envelope>> {
    if input.id.is_nil() || input.tenant_id.is_nil() {
        return Err(KabiPayError::Validation("invalid audit identity".into()));
    }
    // Match PostgreSQL's persisted timestamptz precision before creating both the
    // plaintext audit row and its immutable envelope ordering timestamp.
    let mut input = input.clone();
    input.created_at = chrono::DateTime::from_timestamp_micros(input.created_at.timestamp_micros())
        .ok_or_else(|| KabiPayError::Validation("invalid audit timestamp".into()))?;
    let head = SourceHead {
        record_id: input.id,
        tenant_id: input.tenant_id,
        purpose: "audit".into(),
        source_generation: generation,
        source_kind: "audit_log".into(),
        resource_type: "audit_record".into(),
        resource_id: input.entity_id.unwrap_or(input.id),
        occurred_at: input
            .created_at
            .to_rfc3339_opts(SecondsFormat::Micros, true),
    };
    keys.encrypt_frames(&serde_json::to_vec(&input)?, &head)
}

/// Match the persisted PostgreSQL ordering precision before hashing new intents.
pub(crate) fn canonical_private_time(
    time: chrono::DateTime<chrono::Utc>,
) -> KabiPayResult<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp_micros(time.timestamp_micros())
        .ok_or_else(|| KabiPayError::Validation("invalid private event timestamp".into()))
}
fn canonical_delivery_input(input: &DeliveryIntentInput) -> KabiPayResult<DeliveryIntentInput> {
    let mut input = input.clone();
    input.occurred_at = canonical_private_time(input.occurred_at)?;
    Ok(input)
}

async fn source_generation<C: ConnectionTrait>(db: &C, tenant: Uuid) -> KabiPayResult<i64> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1",
            [tenant.into()],
        ))
        .await?
        .ok_or_else(|| {
            KabiPayError::Conflict("private source authority is not installed".into())
        })?;
    let generation: i64 = row.try_get("", "producer_generation")?;
    if generation < 1 || generation > 9_007_199_254_740_991 {
        return Err(KabiPayError::Internal(
            "invalid private source authority".into(),
        ));
    }
    Ok(generation)
}

const INSERT_FRAMES:&str="INSERT INTO company_event_outbox(id,source_tenant_id,organization_id,event_type,aggregate_type,aggregate_id,aggregate_version,event_envelope,event_stream,created_at) SELECT (e->>'event_id')::uuid,(e->>'tenant_id')::uuid,NULL,'private.'||(e->>'purpose')||'.v1',e->>'resource_type',(e->>'resource_id')::uuid,1,e,e->>'purpose',(e->>'occurred_at')::timestamptz FROM jsonb_array_elements($1::jsonb) e";

/// Append encrypted audit frames in one statement on the caller's connection.
/// An enclosing transaction or savepoint owns this write and its rollback.
pub async fn append_audit<C: ConnectionTrait>(
    db: &C,
    input: &AuditRecordInput,
) -> KabiPayResult<Uuid> {
    append_audit_with_keyring(db, input, &configured_company_keyring(input.tenant_id)?).await
}
pub async fn append_audit_with_keyring<C: ConnectionTrait>(
    db: &C,
    input: &AuditRecordInput,
    keys: &Keyring,
) -> KabiPayResult<Uuid> {
    let generation = source_generation(db, input.tenant_id).await?;
    let frames = audit_frames(input, keys, generation)?;
    let sql=format!("WITH audit_actor AS MATERIALIZED (SELECT u.id FROM \"user\" u WHERE u.id=$4 AND u.tenant_id=$2 FOR KEY SHARE), source_authority AS MATERIALIZED (SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$2 AND producer_generation=$3 AND ($4::uuid IS NULL OR EXISTS(SELECT 1 FROM audit_actor)) FOR SHARE) {INSERT_FRAMES} WHERE EXISTS(SELECT 1 FROM source_authority)");
    let written = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [
                serde_json::to_value(frames)?.into(),
                input.tenant_id.into(),
                generation.into(),
                input.user_id.into(),
            ],
        ))
        .await?;
    if written.rows_affected() == 0 {
        return Err(KabiPayError::Conflict(
            "private audit source authority or tenant actor is unavailable".into(),
        ));
    }
    Ok(input.id)
}

/// Persist one immutable destination receipt and its encrypted frames atomically.
pub async fn enqueue_delivery<C: ConnectionTrait>(
    db: &C,
    input: &DeliveryIntentInput,
) -> KabiPayResult<Uuid> {
    enqueue_delivery_with_keyring(db, input, &configured_company_keyring(input.tenant_id)?).await
}
pub async fn enqueue_delivery_with_keyring<C: ConnectionTrait>(
    db: &C,
    input: &DeliveryIntentInput,
    keys: &Keyring,
) -> KabiPayResult<Uuid> {
    let canonical = canonical_delivery_input(input)?;
    let input = &canonical;
    if [
        input.id,
        input.tenant_id,
        input.destination_id,
        input.resource_id,
    ]
    .iter()
    .any(Uuid::is_nil)
        || input.destination_version < 1
        || !matches!(
            input.source_kind.as_str(),
            "webhook" | "finance_confirmation" | "finance_cancellation"
        )
        || input.event_type.is_empty()
        || input.event_type.len() > 128
    {
        return Err(KabiPayError::Validation("invalid delivery intent".into()));
    }
    let generation = source_generation(db, input.tenant_id).await?;
    let bytes = serde_json::to_vec(input)?;
    let hash = crypto::source_hash(&bytes);
    let head = SourceHead {
        record_id: input.id,
        tenant_id: input.tenant_id,
        purpose: "delivery".into(),
        source_generation: generation,
        source_kind: input.source_kind.clone(),
        resource_type: "delivery_intent".into(),
        resource_id: input.resource_id,
        occurred_at: input
            .occurred_at
            .to_rfc3339_opts(SecondsFormat::Micros, true),
    };
    let frames = keys.encrypt_frames(&bytes, &head)?;
    let sql=format!("WITH source_authority AS MATERIALIZED (SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$2 AND producer_generation=$10 FOR SHARE), inserted_receipt AS (INSERT INTO private_delivery_receipt(tenant_id,intent_id,destination_id,source_kind,resource_id,intent_hash,destination_version,status,created_at,source_generation) SELECT $2,$3,$4,$5,$6,$7,$8,'PENDING',$9,producer_generation FROM source_authority WHERE true ON CONFLICT(tenant_id,intent_id,destination_id) DO NOTHING RETURNING intent_id), inserted_frames AS ({INSERT_FRAMES} WHERE EXISTS(SELECT 1 FROM inserted_receipt) RETURNING id) SELECT (EXISTS(SELECT 1 FROM inserted_receipt) OR EXISTS(SELECT 1 FROM private_delivery_receipt WHERE tenant_id=$2 AND intent_id=$3 AND destination_id=$4 AND intent_hash=$7 AND destination_version=$8 AND source_kind=$5 AND resource_id=$6 AND source_generation=(SELECT producer_generation FROM source_authority))) AS accepted");
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [
                serde_json::to_value(frames)?.into(),
                input.tenant_id.into(),
                input.id.into(),
                input.destination_id.into(),
                input.source_kind.clone().into(),
                input.resource_id.into(),
                hash.into(),
                input.destination_version.into(),
                input.occurred_at.into(),
                generation.into(),
            ],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("delivery receipt not returned".into()))?;
    if !row.try_get::<bool>("", "accepted")? {
        return Err(KabiPayError::Conflict(
            "delivery identity has conflicting content or concurrent ownership".into(),
        ));
    }
    Ok(input.id)
}

/// Authenticated protected history adapters implement this port. Errors, including
/// incomplete coverage, must propagate; unavailable history never means no rows.
pub trait AuditHistoryReader {
    fn read(
        &self,
        filter: &AuditHistoryFilter,
    ) -> impl std::future::Future<Output = KabiPayResult<AuditHistoryPage>> + Send;
}
pub async fn read_audit_history<R: AuditHistoryReader>(
    reader: &R,
    filter: &AuditHistoryFilter,
) -> KabiPayResult<AuditHistoryPage> {
    if filter.tenant_id.is_nil()
        || filter.limit == 0
        || filter.limit > 500
        || filter.offset > 100_000
        || filter.actions.len() > 64
    {
        return Err(KabiPayError::Validation(
            "invalid private history filter".into(),
        ));
    }
    reader.read(filter).await
}

#[cfg(test)]
mod tests {
    use super::crypto::{FRAME_BYTES, MAX_SOURCE_BYTES};
    use super::*;
    use uuid::Uuid;
    const TENANT: &str = "11111111-1111-4111-8111-111111111111";
    #[test]
    fn new_delivery_input_hash_and_head_share_postgres_microsecond_precision() {
        let raw = chrono::DateTime::parse_from_rfc3339("2026-10-19T00:00:00.123456789Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut intent = DeliveryIntentInput {
            id: Uuid::new_v4(),
            tenant_id: Uuid::parse_str(TENANT).unwrap(),
            destination_id: Uuid::new_v4(),
            destination_version: 1,
            source_kind: "webhook".into(),
            resource_id: Uuid::new_v4(),
            event_type: "expense.approved".into(),
            payload: serde_json::json!({"original":true}),
            occurred_at: raw,
        };
        let first = canonical_delivery_input(&intent).unwrap();
        assert_eq!(
            first
                .occurred_at
                .to_rfc3339_opts(SecondsFormat::Micros, true),
            "2026-10-19T00:00:00.123456Z"
        );
        intent.occurred_at = chrono::DateTime::parse_from_rfc3339("2026-10-19T00:00:00.123456001Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let second = canonical_delivery_input(&intent).unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert_eq!(first.payload, intent.payload);
    }
    fn keyring() -> Keyring {
        Keyring::from_json(format!(r#"{{"schema_version":1,"keys":[{{"tenant_id":"{TENANT}","purpose":"audit","key_id":"test-v1","key_b64":"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=","active":true}}]}}"#).as_bytes()).unwrap()
    }
    fn head() -> SourceHead {
        SourceHead {
            record_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
            tenant_id: Uuid::parse_str(TENANT).unwrap(),
            purpose: "audit".into(),
            source_generation: 17,
            source_kind: "audit_log".into(),
            resource_type: "UNKNOWN".into(),
            resource_id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
            occurred_at: "2026-10-19T00:00:00.123456Z".into(),
        }
    }
    #[test]
    fn exact_source_bytes_roundtrip_and_tamper_rejection() {
        let keys = keyring();
        let h = head();
        let bytes = "{\"unknown\":\"é\",\"before\":null}\n".as_bytes();
        let f = keys.encrypt_frames(bytes, &h).unwrap();
        assert_eq!(
            keys.decrypt_frames(&f, h.tenant_id, "audit").unwrap(),
            bytes
        );
        let mut bad = f.clone();
        bad[0].source_kind = "other".into();
        assert!(keys.decrypt_frames(&bad, h.tenant_id, "audit").is_err());
        let mut stale = f.clone();
        stale[0].source_generation = 18;
        assert!(keys.decrypt_frames(&stale, h.tenant_id, "audit").is_err());
        assert!(keys.decrypt_frames(&f, Uuid::new_v4(), "audit").is_err());
        assert!(keys.decrypt_frames(&f, h.tenant_id, "delivery").is_err());
    }
    #[test]
    fn new_audit_plaintext_and_head_share_postgres_microsecond_precision() {
        let original = chrono::DateTime::from_timestamp(1_792_368_000, 123_456_789).unwrap();
        let input = AuditRecordInput {
            id: head().record_id,
            tenant_id: head().tenant_id,
            user_id: None,
            entity_type: "UNKNOWN".into(),
            entity_id: None,
            action: "UNKNOWN".into(),
            before_state: Some(serde_json::json!({"unknown":"exact"})),
            after_state: None,
            ip_address: None,
            user_agent: None,
            created_at: original,
        };
        let frames = audit_frames(&input, &keyring(), 17).unwrap();
        let restored: AuditRecordInput = serde_json::from_slice(
            &keyring()
                .decrypt_frames(&frames, input.tenant_id, "audit")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(restored.created_at.timestamp_subsec_nanos(), 123_456_000);
        assert_eq!(
            restored.created_at.timestamp_micros(),
            original.timestamp_micros()
        );
        assert_eq!(restored.before_state, input.before_state);
        assert_eq!(
            frames[0].occurred_at,
            restored
                .created_at
                .to_rfc3339_opts(SecondsFormat::Micros, true)
        );
        assert_eq!(input.created_at, original);
    }
    #[test]
    fn complete_framing_required() {
        let keys = keyring();
        let h = head();
        let bytes = vec![3; FRAME_BYTES + 1];
        let f = keys.encrypt_frames(&bytes, &h).unwrap();
        assert_eq!(f.len(), 2);
        assert!(keys.decrypt_frames(&f[..1], h.tenant_id, "audit").is_err());
        assert!(keys
            .decrypt_frames(&[f[0].clone(), f[0].clone()], h.tenant_id, "audit")
            .is_err());
        assert_eq!(
            keys.decrypt_frames(&f, h.tenant_id, "audit").unwrap(),
            bytes
        );
        assert!(keys
            .encrypt_frames(&vec![0; MAX_SOURCE_BYTES + 1], &h)
            .is_err());
    }
    #[test]
    fn keyring_has_no_implicit_key() {
        assert!(Keyring::from_json(b"{}").is_err());
        assert!(Keyring::from_json(br#"{"schema_version":1,"keys":[]}"#).is_err());
    }
    #[test]
    fn node_frozen_vector_decrypts_exactly_in_rust() {
        use base64::Engine;
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../integrations/organization-events/test/private-vector.json"
        ))
        .unwrap();
        let keys = Keyring::from_json(&serde_json::to_vec(&v["keyfile"]).unwrap()).unwrap();
        let frames: Vec<Envelope> = serde_json::from_value(v["frames"].clone()).unwrap();
        let source = base64::engine::general_purpose::STANDARD
            .decode(v["source_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            keys.decrypt_frames(&frames, head().tenant_id, "audit")
                .unwrap(),
            source
        );
    }
    #[tokio::test]
    #[ignore = "requires explicit isolated PRIVATE_EVENT_TEST_DATABASE_URL"]
    async fn postgres_transaction_savepoint_generation_and_delivery_atomicity() {
        use sea_orm::{Database, TransactionTrait};
        let db = Database::connect(
            std::env::var("PRIVATE_EVENT_TEST_DATABASE_URL").expect("isolated DB URL required"),
        )
        .await
        .unwrap();
        let tx = db.begin().await.unwrap();
        for sql in [
            "CREATE TEMP TABLE \"user\"(id uuid PRIMARY KEY,tenant_id uuid NOT NULL) ON COMMIT DROP",
            "CREATE TEMP TABLE private_source_cutover(tenant_id uuid PRIMARY KEY, producer_generation bigint NOT NULL) ON COMMIT DROP",
            "CREATE TEMP TABLE private_delivery_receipt(tenant_id uuid,intent_id uuid,destination_id uuid,source_kind text,resource_id uuid,intent_hash char(64),destination_version bigint,status text,created_at timestamptz,source_generation bigint,PRIMARY KEY(tenant_id,intent_id,destination_id)) ON COMMIT DROP",
            "CREATE TEMP TABLE company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid,organization_id uuid,event_type text,aggregate_type text,aggregate_id uuid,aggregate_version bigint,event_envelope jsonb NOT NULL,event_stream text,created_at timestamptz) ON COMMIT DROP",
        ] {tx.execute_unprepared(sql).await.unwrap();}
        let tenant = head().tenant_id;
        let a = AuditRecordInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            user_id: None,
            entity_type: "UNKNOWN_ENTITY".into(),
            entity_id: None,
            action: "UNKNOWN_ACTION".into(),
            before_state: Some(serde_json::json!({"unknown":"é"})),
            after_state: None,
            ip_address: Some("192.0.2.1".into()),
            user_agent: Some("synthetic".into()),
            created_at: chrono::Utc::now(),
        };
        assert!(append_audit_with_keyring(&tx, &a, &keyring())
            .await
            .is_err());
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO private_source_cutover VALUES($1,17)",
            [tenant.into()],
        ))
        .await
        .unwrap();
        let savepoint = tx.begin().await.unwrap();
        append_audit_with_keyring(&savepoint, &a, &keyring())
            .await
            .unwrap();
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
        append_audit_with_keyring(&tx, &a, &keyring())
            .await
            .unwrap();
        let actor = Uuid::new_v4();
        let other_actor = Uuid::new_v4();
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO \"user\" VALUES($1,$2),($3,$4)",
            [
                actor.into(),
                tenant.into(),
                other_actor.into(),
                Uuid::new_v4().into(),
            ],
        ))
        .await
        .unwrap();
        let mut attributed = a.clone();
        attributed.id = Uuid::new_v4();
        attributed.user_id = Some(Uuid::new_v4());
        assert!(append_audit_with_keyring(&tx, &attributed, &keyring())
            .await
            .is_err());
        attributed.user_id = Some(other_actor);
        assert!(append_audit_with_keyring(&tx, &attributed, &keyring())
            .await
            .is_err());
        attributed.user_id = Some(actor);
        append_audit_with_keyring(&tx, &attributed, &keyring())
            .await
            .unwrap();
        let row = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT event_envelope FROM company_event_outbox",
            ))
            .await
            .unwrap()
            .unwrap();
        let envelope: Envelope =
            serde_json::from_value(row.try_get("", "event_envelope").unwrap()).unwrap();
        assert_eq!(envelope.source_generation, 17);
        let bytes = keyring()
            .decrypt_frames(&[envelope], tenant, "audit")
            .unwrap();
        let restored: AuditRecordInput = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.action, a.action);
        assert_eq!(restored.before_state, a.before_state);
        let ring=Keyring::from_json(format!(r#"{{"schema_version":1,"keys":[{{"tenant_id":"{TENANT}","purpose":"delivery","key_id":"test-v1","key_b64":"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=","active":true}}]}}"#).as_bytes()).unwrap();
        let intent = DeliveryIntentInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            destination_id: Uuid::new_v4(),
            destination_version: 1,
            source_kind: "webhook".into(),
            resource_id: Uuid::new_v4(),
            event_type: "employee.created".into(),
            payload: serde_json::json!({"synthetic":true}),
            occurred_at: chrono::Utc::now(),
        };
        enqueue_delivery_with_keyring(&tx, &intent, &ring)
            .await
            .unwrap();
        enqueue_delivery_with_keyring(&tx, &intent, &ring)
            .await
            .unwrap();
        let mut conflicting = intent.clone();
        conflicting.payload = serde_json::json!({"different":true});
        assert!(enqueue_delivery_with_keyring(&tx, &conflicting, &ring)
            .await
            .is_err());
        let count = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS n FROM private_delivery_receipt",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(count, 1);
        tx.execute_unprepared("ALTER TABLE company_event_outbox ADD CONSTRAINT block_test_delivery CHECK(event_stream <> 'delivery') NOT VALID").await.unwrap();
        let savepoint = tx.begin().await.unwrap();
        let mut failed = intent.clone();
        failed.id = Uuid::new_v4();
        assert!(enqueue_delivery_with_keyring(&savepoint, &failed, &ring)
            .await
            .is_err());
        savepoint.rollback().await.unwrap();
        let count = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS n FROM private_delivery_receipt",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(count, 1);
        tx.execute_unprepared("UPDATE private_source_cutover SET producer_generation=18")
            .await
            .unwrap();
        assert!(enqueue_delivery_with_keyring(&tx, &intent, &ring)
            .await
            .is_err());
        tx.rollback().await.unwrap();
    }
}
