//! Authenticated acknowledgement of durable broker ownership, never receiver delivery.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use kabipay_common::{private_events::crypto::source_hash, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipRequest {
    pub version: u8,
    pub tenant_id: Uuid,
    pub intent_id: Uuid,
    pub destination_id: Uuid,
    pub source_generation: i64,
    pub destination_version: i64,
    pub intent_hash: String,
    pub broker_owner_id: Uuid,
    pub topic: String,
    pub partition_id: u32,
    pub kafka_offset: String,
    pub request_id: Uuid,
    pub issued_at: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedOwnershipRequest {
    pub request: OwnershipRequest,
    pub signature: String,
}
#[derive(Serialize, Deserialize)]
pub struct OwnershipResponse {
    #[serde(flatten)]
    pub request: OwnershipRequest,
    pub receipt_sha256: String,
    pub accepted_at: i64,
    pub already_owned: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedOwnershipResponse {
    pub response: OwnershipResponse,
    pub signature: String,
}
fn denied() -> KabiPayError {
    KabiPayError::Forbidden("Private broker ownership proof rejected".into())
}
fn signature<T: Serialize>(domain: &[u8], body: &T, key: &[u8]) -> KabiPayResult<Hmac<Sha256>> {
    if key.len() < 32 {
        return Err(denied());
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| denied())?;
    mac.update(domain);
    mac.update(&serde_json::to_vec(body)?);
    Ok(mac)
}
pub fn verify_request(
    envelope: &SignedOwnershipRequest,
    tenant: Uuid,
    generation: i64,
    owner: Uuid,
    key: &[u8],
    now: i64,
) -> KabiPayResult<()> {
    let r = &envelope.request;
    if r.version != 1
        || r.tenant_id != tenant
        || r.source_generation != generation
        || r.source_generation < 1
        || r.source_generation > 9_007_199_254_740_991
        || r.destination_version < 1
        || r.destination_version > 9_007_199_254_740_991
        || [
            r.tenant_id,
            r.intent_id,
            r.destination_id,
            r.broker_owner_id,
            r.request_id,
        ]
        .iter()
        .any(Uuid::is_nil)
        || r.broker_owner_id != owner
        || r.topic != format!("organization.delivery.work.{}", r.tenant_id)
        || r.partition_id != 0
        || r.kafka_offset.is_empty()
        || r.kafka_offset.len() > 19
        || !r.kafka_offset.bytes().all(|v| v.is_ascii_digit())
        || r.kafka_offset.parse::<i64>().is_err()
        || r.intent_hash.len() != 64
        || !r
            .intent_hash
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        || r.issued_at < now.saturating_sub(60)
        || r.issued_at > now.saturating_add(5)
    {
        return Err(denied());
    }
    let sig = URL_SAFE_NO_PAD
        .decode(&envelope.signature)
        .map_err(|_| denied())?;
    if URL_SAFE_NO_PAD.encode(&sig) != envelope.signature {
        return Err(denied());
    }
    signature(b"kabipay-private-ownership-request-v1\0", r, key)?
        .verify_slice(&sig)
        .map_err(|_| denied())
}
pub fn authorize(envelope: &SignedOwnershipRequest) -> KabiPayResult<()> {
    let (generation, key) = super::private_delivery::ownership_secret(envelope.request.tenant_id)?;
    let owner = std::env::var("KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(denied)?;
    verify_request(
        envelope,
        envelope.request.tenant_id,
        generation,
        owner,
        &key,
        Utc::now().timestamp(),
    )
}
pub fn receipt_hash(r: &OwnershipRequest) -> KabiPayResult<String> {
    // Serializing the ordered request then dropping only its last two fields
    // preserves the shared compact JSON field order without a map canonicalizer.
    let json = serde_json::to_string(r)?;
    let end = json.rfind(",\"request_id\":").ok_or_else(denied)?;
    Ok(source_hash(format!("{}}}", &json[..end]).as_bytes()))
}
pub async fn acknowledge(
    db: &DatabaseConnection,
    envelope: &SignedOwnershipRequest,
) -> KabiPayResult<SignedOwnershipResponse> {
    authorize(envelope)?;
    let r = &envelope.request;
    let (_, key) = super::private_delivery::ownership_secret(r.tenant_id)?;
    let tx = db.begin().await?;
    let authority = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1 FOR SHARE",
            [r.tenant_id.into()],
        ))
        .await?
        .ok_or_else(denied)?;
    if authority.try_get::<i64>("", "producer_generation")? != r.source_generation {
        return Err(denied());
    }
    let row=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT intent_hash,destination_version,source_generation,broker_owned_at,broker_owner_id,broker_receipt_sha256 FROM private_delivery_receipt WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 FOR UPDATE",[r.tenant_id.into(),r.intent_id.into(),r.destination_id.into()])).await?.ok_or_else(denied)?;
    if row.try_get::<String>("", "intent_hash")? != r.intent_hash
        || row.try_get::<i64>("", "destination_version")? != r.destination_version
        || row.try_get::<i64>("", "source_generation")? != r.source_generation
    {
        return Err(denied());
    }
    let prior: Option<DateTime<Utc>> = row.try_get("", "broker_owned_at")?;
    let already_owned = prior.is_some();
    let (accepted_at, receipt_sha256) = if let Some(at) = prior {
        if row.try_get::<Option<Uuid>>("", "broker_owner_id")? != Some(r.broker_owner_id) {
            return Err(denied());
        }
        (
            at.timestamp(),
            row.try_get::<String>("", "broker_receipt_sha256")?,
        )
    } else {
        let now = Utc::now();
        let digest = receipt_hash(r)?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET broker_owned_at=$4,broker_owner_id=$5,broker_receipt_sha256=$6,updated_at=clock_timestamp() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND broker_owned_at IS NULL",[r.tenant_id.into(),r.intent_id.into(),r.destination_id.into(),now.into(),r.broker_owner_id.into(),digest.clone().into()])).await?;
        (now.timestamp(), digest)
    };
    tx.commit().await?;
    let response = OwnershipResponse {
        request: r.clone(),
        receipt_sha256,
        accepted_at,
        already_owned,
    };
    let signature = URL_SAFE_NO_PAD.encode(
        signature(b"kabipay-private-ownership-response-v1\0", &response, &key)?
            .finalize()
            .into_bytes(),
    );
    Ok(SignedOwnershipResponse {
        response,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires explicit disposable PRIVATE_LEGACY_TEST_DATABASE_URL"]
    async fn postgres_broker_ownership_is_idempotent_and_never_delivery_acknowledgement() {
        use sea_orm::{ConnectOptions, Database};
        let url = std::env::var("PRIVATE_LEGACY_TEST_DATABASE_URL")
            .expect("isolated source fixture URL required");
        assert!(
            url.starts_with("postgresql://private_fixture:private_fixture@127.0.0.1:")
                && url.ends_with("/private_retirement_fixture"),
            "refusing a non-fixture database"
        );
        let schema = format!("ownership_test_{}", Uuid::new_v4().simple());
        let mut options = ConnectOptions::new(url);
        options
            .max_connections(1)
            .sqlx_logging(false)
            .set_schema_search_path(format!("{schema},public"));
        let db = Database::connect(options).await.unwrap();
        db.execute_unprepared(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.private_source_cutover(tenant_id uuid PRIMARY KEY,producer_generation bigint NOT NULL,source_database text NOT NULL DEFAULT current_database()); CREATE TABLE {schema}.private_delivery_receipt(tenant_id uuid,intent_id uuid,destination_id uuid,intent_hash char(64),destination_version bigint,source_generation bigint,broker_owned_at timestamptz,broker_owner_id uuid,broker_receipt_sha256 char(64),updated_at timestamptz,status text DEFAULT 'PENDING',accepted_at timestamptz,lease_generation bigint NOT NULL DEFAULT 0,lease_until timestamptz,claim_owner uuid,acknowledgement_id uuid,outcome_code text,outcome_sha256 char(64),source_kind text NOT NULL DEFAULT 'webhook',resource_id uuid,PRIMARY KEY(tenant_id,intent_id,destination_id));")).await.unwrap();
        let migration=include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../../hrms-database/changelog/migrations/0125_private_event_source/private_event_source.xml"));
        let begin = migration
            .find("CREATE FUNCTION \"${schema}\".private_receipt_guard()")
            .unwrap();
        let function = &migration[begin..];
        let end = function.find("END $fn$;").unwrap() + "END $fn$;".len();
        db.execute_unprepared(&function[..end].replace("${schema}", &schema))
            .await
            .unwrap();
        db.execute_unprepared(&format!("CREATE TRIGGER private_receipt_guard BEFORE UPDATE OR DELETE ON {schema}.private_delivery_receipt FOR EACH ROW EXECUTE FUNCTION {schema}.private_receipt_guard()")).await.unwrap();
        let tenant = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let config =
            std::env::temp_dir().join(format!("private-ownership-{}.json", Uuid::new_v4()));
        std::fs::write(&config,serde_json::to_vec(&serde_json::json!({"schema_version":1,"bindings":[{"tenant_id":tenant,"source_generation":1,"key_b64":base64::engine::general_purpose::STANDARD.encode([7;32]),"allowed_webhook_hosts":[],"webhook_keys":[]}]})).unwrap()).unwrap();
        let old_file = std::env::var_os("KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE");
        let old_owner = std::env::var_os("KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID");
        std::env::set_var("KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE", &config);
        std::env::set_var(
            "KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID",
            owner.to_string(),
        );
        let request = OwnershipRequest {
            version: 1,
            tenant_id: tenant,
            intent_id: Uuid::new_v4(),
            destination_id: Uuid::new_v4(),
            source_generation: 1,
            destination_version: 1,
            intent_hash: "a".repeat(64),
            broker_owner_id: owner,
            topic: format!("organization.delivery.work.{tenant}"),
            partition_id: 0,
            kafka_offset: "9223372036854775800".into(),
            request_id: Uuid::new_v4(),
            issued_at: Utc::now().timestamp(),
        };
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO private_source_cutover(tenant_id,producer_generation) VALUES($1,1)",
            [tenant.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_delivery_receipt(tenant_id,intent_id,destination_id,intent_hash,destination_version,source_generation) VALUES($1,$2,$3,$4,1,1)",[tenant.into(),request.intent_id.into(),request.destination_id.into(),request.intent_hash.clone().into()])).await.unwrap();
        let signed = |request: OwnershipRequest| {
            let signature = URL_SAFE_NO_PAD.encode(
                signature(
                    b"kabipay-private-ownership-request-v1\0",
                    &request,
                    &[7; 32],
                )
                .unwrap()
                .finalize()
                .into_bytes(),
            );
            SignedOwnershipRequest { request, signature }
        };
        let first = acknowledge(&db, &signed(request.clone())).await.unwrap();
        assert!(!first.response.already_owned);
        assert_eq!(
            first.response.receipt_sha256,
            receipt_hash(&request).unwrap()
        );
        let mut duplicate = request.clone();
        duplicate.kafka_offset = "9223372036854775801".into();
        duplicate.request_id = Uuid::new_v4();
        let second = acknowledge(&db, &signed(duplicate)).await.unwrap();
        assert!(second.response.already_owned);
        assert_eq!(
            first.response.receipt_sha256,
            second.response.receipt_sha256
        );
        assert_eq!(first.response.accepted_at, second.response.accepted_at);
        // A migrated acknowledgement and an execute-before-ownership outcome
        // both remain terminal while the collector records first broker ownership.
        for outcome in [
            "legacy_correlated_v1_http_2xx",
            "executed_before_broker_ownership",
        ] {
            let mut terminal = request.clone();
            terminal.intent_id = Uuid::new_v4();
            terminal.destination_id = Uuid::new_v4();
            terminal.request_id = Uuid::new_v4();
            let ack = Uuid::new_v4();
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_delivery_receipt(tenant_id,intent_id,destination_id,intent_hash,destination_version,source_generation,status,accepted_at,acknowledgement_id,outcome_code,resource_id) VALUES($1,$2,$3,$4,1,1,'DELIVERED',clock_timestamp(),$5,$6,$2)",[tenant.into(),terminal.intent_id.into(),terminal.destination_id.into(),terminal.intent_hash.clone().into(),ack.into(),outcome.into()])).await.unwrap();
            let snapshot_sql="SELECT to_jsonb(t)-ARRAY['broker_owned_at','broker_owner_id','broker_receipt_sha256','updated_at'] AS original FROM private_delivery_receipt t WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3";
            let before = db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    snapshot_sql,
                    [
                        tenant.into(),
                        terminal.intent_id.into(),
                        terminal.destination_id.into(),
                    ],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<serde_json::Value>("", "original")
                .unwrap();
            let delivery = super::super::private_delivery::DeliveryRequest {
                version: 1,
                tenant_id: tenant,
                intent_id: terminal.intent_id,
                destination_id: terminal.destination_id,
                source_generation: 1,
                destination_version: 1,
                intent_hash: terminal.intent_hash.clone(),
                lease_generation: 1,
                request_id: Uuid::new_v4(),
                issued_at: Utc::now().timestamp(),
            };
            let delivery_sig = URL_SAFE_NO_PAD.encode(
                signature(
                    b"kabipay-private-delivery-request-v1\0",
                    &delivery,
                    &[7; 32],
                )
                .unwrap()
                .finalize()
                .into_bytes(),
            );
            let delivery = super::super::private_delivery::SignedDeliveryRequest {
                request: delivery,
                signature: delivery_sig,
            };
            let no_resend = super::super::private_delivery::execute(&db, &db, &delivery)
                .await
                .unwrap();
            assert_eq!(no_resend.response.status, "DELIVERED");
            assert_eq!(no_resend.response.acknowledgement_id, Some(ack));
            let first = acknowledge(&db, &signed(terminal.clone())).await.unwrap();
            assert!(!first.response.already_owned);
            terminal.kafka_offset = "9223372036854775801".into();
            terminal.request_id = Uuid::new_v4();
            let duplicate = acknowledge(&db, &signed(terminal.clone())).await.unwrap();
            assert!(duplicate.response.already_owned);
            assert_eq!(
                duplicate.response.receipt_sha256,
                first.response.receipt_sha256
            );
            let no_resend = super::super::private_delivery::execute(&db, &db, &delivery)
                .await
                .unwrap();
            assert_eq!(no_resend.response.status, "DELIVERED");
            assert_eq!(no_resend.response.acknowledgement_id, Some(ack));
            let after = db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    snapshot_sql,
                    [
                        tenant.into(),
                        terminal.intent_id.into(),
                        terminal.destination_id.into(),
                    ],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<serde_json::Value>("", "original")
                .unwrap();
            assert_eq!(before, after);
            assert!(db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET outcome_code='changed' WHERE tenant_id=$1 AND intent_id=$2",[tenant.into(),terminal.intent_id.into()])).await.is_err());
            assert!(db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET broker_owner_id=$3 WHERE tenant_id=$1 AND intent_id=$2",[tenant.into(),terminal.intent_id.into(),Uuid::new_v4().into()])).await.is_err());
        }
        for change in 0..4 {
            let mut wrong = request.clone();
            match change {
                0 => wrong.intent_hash = "b".repeat(64),
                1 => wrong.destination_id = Uuid::new_v4(),
                2 => wrong.broker_owner_id = Uuid::new_v4(),
                _ => wrong.source_generation = 2,
            };
            assert!(acknowledge(&db, &signed(wrong)).await.is_err());
        }
        let status = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,accepted_at FROM private_delivery_receipt WHERE tenant_id=$1 AND intent_id=$2",
                [tenant.into(),request.intent_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.try_get::<String>("", "status").unwrap(), "PENDING");
        assert!(status
            .try_get::<Option<DateTime<Utc>>>("", "accepted_at")
            .unwrap()
            .is_none());
        db.execute_unprepared("UPDATE private_source_cutover SET producer_generation=2")
            .await
            .unwrap();
        assert!(acknowledge(&db, &signed(request)).await.is_err());
        match old_file {
            Some(v) => std::env::set_var("KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE", v),
            None => std::env::remove_var("KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE"),
        };
        match old_owner {
            Some(v) => std::env::set_var("KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID", v),
            None => std::env::remove_var("KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID"),
        };
        std::fs::remove_file(config).unwrap();
        db.execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
        db.close().await.unwrap();
    }
    #[test]
    fn node_rust_ownership_wire_and_receipt_hash_are_identical() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../integrations/organization-events/test/private-ownership-vector.json"
        )))
        .unwrap();
        let key = base64::engine::general_purpose::STANDARD
            .decode(fixture["key_b64"].as_str().unwrap())
            .unwrap();
        let envelope: SignedOwnershipRequest =
            serde_json::from_value(fixture["request"].clone()).unwrap();
        assert_eq!(
            serde_json::to_string(&envelope.request).unwrap(),
            fixture["request_json"].as_str().unwrap()
        );
        verify_request(
            &envelope,
            envelope.request.tenant_id,
            envelope.request.source_generation,
            envelope.request.broker_owner_id,
            &key,
            envelope.request.issued_at,
        )
        .unwrap();
        assert_eq!(
            receipt_hash(&envelope.request).unwrap(),
            fixture["receipt_sha256"].as_str().unwrap()
        );
        let response: SignedOwnershipResponse =
            serde_json::from_value(fixture["response"].clone()).unwrap();
        assert_eq!(
            serde_json::to_string(&response.response).unwrap(),
            fixture["response_json"].as_str().unwrap()
        );
        signature(
            b"kabipay-private-ownership-response-v1\0",
            &response.response,
            &key,
        )
        .unwrap()
        .verify_slice(&URL_SAFE_NO_PAD.decode(response.signature).unwrap())
        .unwrap();
    }
    #[test]
    fn ownership_proof_is_scoped_fresh_and_binds_every_broker_coordinate() {
        let request = OwnershipRequest {
            version: 1,
            tenant_id: Uuid::from_u128(1),
            intent_id: Uuid::from_u128(2),
            destination_id: Uuid::from_u128(3),
            source_generation: 1,
            destination_version: 1,
            intent_hash: "a".repeat(64),
            broker_owner_id: Uuid::from_u128(4),
            topic: format!("organization.delivery.work.{}", Uuid::from_u128(1)),
            partition_id: 0,
            kafka_offset: "9007199254740999".into(),
            request_id: Uuid::from_u128(5),
            issued_at: 100,
        };
        let sign = |r: &OwnershipRequest| {
            URL_SAFE_NO_PAD.encode(
                signature(b"kabipay-private-ownership-request-v1\0", r, &[7; 32])
                    .unwrap()
                    .finalize()
                    .into_bytes(),
            )
        };
        let proof = SignedOwnershipRequest {
            signature: sign(&request),
            request: request.clone(),
        };
        assert!(verify_request(
            &proof,
            request.tenant_id,
            1,
            request.broker_owner_id,
            &[7; 32],
            100
        )
        .is_ok());
        for bad in [101i64, 161] {
            assert!(verify_request(
                &proof,
                request.tenant_id,
                1,
                request.broker_owner_id,
                &[7; 32],
                bad + 60
            )
            .is_err());
        }
        let mut changed = request.clone();
        changed.kafka_offset = "9007199254741000".into();
        assert!(verify_request(
            &SignedOwnershipRequest {
                request: changed,
                signature: proof.signature.clone()
            },
            request.tenant_id,
            1,
            request.broker_owner_id,
            &[7; 32],
            100
        )
        .is_err());
        assert!(verify_request(
            &proof,
            request.tenant_id,
            2,
            request.broker_owner_id,
            &[7; 32],
            100
        )
        .is_err());
        assert!(
            verify_request(&proof, request.tenant_id, 1, Uuid::new_v4(), &[7; 32], 100).is_err()
        );
        let first = receipt_hash(&request).unwrap();
        let mut retry = request;
        retry.request_id = Uuid::new_v4();
        retry.issued_at = 101;
        assert_eq!(first, receipt_hash(&retry).unwrap());
        retry.kafka_offset = "9007199254741000".into();
        assert_ne!(first, receipt_hash(&retry).unwrap());
    }
}
