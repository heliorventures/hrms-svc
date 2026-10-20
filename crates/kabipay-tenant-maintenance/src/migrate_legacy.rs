//! Explicit source operator migration. Plans contain identities and decisions;
//! business payloads, signing keys and destinations are always reconstructed locally.
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use kabipay_common::{
    private_events::{self, DeliveryIntentInput},
    private_webhooks::{self, LegacyWebhookEvent},
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use uuid::Uuid;

const PAYROLL: Uuid = Uuid::from_u128(0x00000000000040008000000000000002);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u8,
    tenant_id: Uuid,
    source_generation: i64,
    accepted_by: String,
    entries: Vec<Entry>,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    legacy_table: String,
    legacy_id: Uuid,
    manifest_id: Uuid,
    source_row_sha256: String,
    classification: String,
    reason: String,
    #[serde(default)]
    authorize_retry_uncertain: bool,
    #[serde(default)]
    expected_destinations: Vec<Destination>,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    destination_id: Uuid,
    destination_version: i64,
}
fn field_uuid(row: &Value, key: &str) -> anyhow::Result<Uuid> {
    Ok(row[key].as_str().context("Missing source UUID")?.parse()?)
}
fn timestamp(row: &Value, key: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(
        DateTime::parse_from_rfc3339(row[key].as_str().context("Missing source timestamp")?)?
            .with_timezone(&Utc),
    )
}
fn valid_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn validate(plan: &Plan, bound: Uuid) -> anyhow::Result<()> {
    if plan.version != 1
        || plan.tenant_id != bound
        || plan.source_generation < 1
        || plan.source_generation > 9_007_199_254_740_991
        || plan.accepted_by.trim().is_empty()
        || plan.accepted_by.len() > 128
        || plan.entries.is_empty()
        || plan.entries.len() > 100
    {
        bail!("Invalid source migration plan");
    }
    let mut identities = std::collections::HashSet::new();
    for e in &plan.entries {
        if ![
            "outbox_event",
            "webhook_delivery_log",
            "employee_finance_outbox",
        ]
        .contains(&e.legacy_table.as_str())
            || e.legacy_id.is_nil()
            || e.manifest_id.is_nil()
            || !valid_hash(&e.source_row_sha256)
            || !identities.insert((&e.legacy_table, e.legacy_id))
            || e.reason.trim().len() < 8
            || e.reason.len() > 512
            || e.expected_destinations.len() > 1001
            || ![
                "HISTORY_ONLY",
                "TRANSFERRED",
                "ACKNOWLEDGED",
                "RECONCILED_NONEXECUTABLE",
            ]
            .contains(&e.classification.as_str())
        {
            bail!("Invalid explicit source migration resolution");
        }
        let mut destinations = std::collections::HashSet::new();
        for d in &e.expected_destinations {
            if d.destination_id.is_nil()
                || d.destination_version < 1
                || !destinations.insert(d.destination_id)
            {
                bail!("Duplicate/invalid destination resolution");
            }
        }
        if e.classification == "TRANSFERRED" && e.expected_destinations.is_empty() {
            bail!("Transfer requires exact expected destination set");
        }
        if e.classification != "TRANSFERRED" && !e.expected_destinations.is_empty() {
            bail!("Nonexecutable resolution cannot invent destination ownership");
        }
    }
    Ok(())
}
fn history_only(table: &str, row: &Value) -> bool {
    match table {
        "outbox_event" => row["status"] == "PROCESSED",
        "employee_finance_outbox" => row["delivered_at"].is_string(),
        _ => false,
    }
}
fn ack_matches(log: &Value, event: &Value, tenant: Uuid, destination: Uuid) -> bool {
    let p = &log["payload_json"];
    log["tenant_id"].as_str() == Some(tenant.to_string().as_str())
        && log["webhook_subscription_id"].as_str() == Some(destination.to_string().as_str())
        && log["is_success"] == true
        && log["http_status"]
            .as_i64()
            .is_some_and(|v| (200..300).contains(&v))
        && p["schema_version"] == 1
        && p["outbox_id"] == event["id"]
        && p["tenant_id"] == event["tenant_id"]
        && p["aggregate_type"] == event["aggregate_type"]
        && p["aggregate_id"] == event["aggregate_id"]
        && p["event_type"] == event["event_type"]
        && p["payload"] == event["payload"]
        && timestamp(p, "created_at").ok() == timestamp(event, "created_at").ok()
        && timestamp(p, "created_at").is_ok()
}
async fn source_row(
    tx: &DatabaseTransaction,
    table: &str,
    id: Uuid,
    tenant: Uuid,
) -> anyhow::Result<(Value, String)> {
    let r=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT to_jsonb(t) AS row,encode(sha256(convert_to(to_jsonb(t)::text,'UTF8')),'hex') AS hash FROM {table} t WHERE id=$1 AND tenant_id=$2 FOR SHARE"),[id.into(),tenant.into()])).await?.context("Legacy source row missing")?;
    Ok((r.try_get("", "row")?, r.try_get("", "hash")?))
}
async fn ensure_ack_log(tx: &DatabaseTransaction, row: &Value, tenant: Uuid) -> anyhow::Result<()> {
    let id = field_uuid(&row["payload_json"], "outbox_id")?;
    let (event, _) = source_row(tx, "outbox_event", id, tenant).await?;
    if !ack_matches(
        row,
        &event,
        tenant,
        field_uuid(row, "webhook_subscription_id")?,
    ) {
        bail!("Webhook acknowledgement lacks exact stored v1 source correlation");
    }
    let exact=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT l.payload_json->'payload'=o.payload AS exact FROM webhook_delivery_log l JOIN outbox_event o ON o.tenant_id=l.tenant_id AND o.id=$1 WHERE l.tenant_id=$2 AND l.id=$3",[id.into(),tenant.into(),field_uuid(row,"id")?.into()])).await?.context("Correlated acknowledgement disappeared")?;
    if !exact.try_get::<bool>("", "exact")? {
        bail!("Stored webhook payload differs from exact PostgreSQL source JSONB");
    }
    Ok(())
}
async fn transfer(
    tx: &DatabaseTransaction,
    plan: &Plan,
    e: &Entry,
    row: &Value,
) -> anyhow::Result<Vec<(Uuid, Uuid, String)>> {
    if history_only(&e.legacy_table, row) || e.legacy_table == "webhook_delivery_log" {
        bail!("Historical delivery cannot become executable work");
    }
    let uncertain = if e.legacy_table == "outbox_event" {
        row["status"] != "PENDING" || row["retry_count"].as_i64().unwrap_or(0) > 0
    } else {
        row["attempts"].as_i64().unwrap_or(0) > 0
    };
    if e.legacy_table == "outbox_event" {
        private_webhooks::enqueue_webhooks(
            tx,
            LegacyWebhookEvent {
                id: e.legacy_id,
                tenant_id: plan.tenant_id,
                aggregate_type: row["aggregate_type"]
                    .as_str()
                    .context("Missing aggregate type")?
                    .into(),
                aggregate_id: field_uuid(row, "aggregate_id")?,
                event_type: row["event_type"]
                    .as_str()
                    .context("Missing event type")?
                    .into(),
                payload: row["payload"].clone(),
                created_at: timestamp(row, "created_at")?,
            },
        )
        .await?;
    } else {
        let operation = field_uuid(row, "operation_id")?;
        let event = row["event_type"]
            .as_str()
            .context("Missing Finance event kind")?;
        let (kind, state) = match event {
            "EXIT_APPROVAL_COMMITTED" => ("finance_confirmation", "COMMITTED"),
            "EXIT_APPROVAL_CANCELLED" => ("finance_cancellation", "CANCELLED"),
            _ => bail!("Unknown Finance terminal event"),
        };
        let actual = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT state FROM employee_exit_operation WHERE tenant_id=$1 AND id=$2 FOR SHARE",
                [plan.tenant_id.into(), operation.into()],
            ))
            .await?
            .context("Finance operation missing")?;
        if actual.try_get::<String>("", "state")? != state
            || row["payload"]["operationId"].as_str() != Some(operation.to_string().as_str())
        {
            bail!("Finance terminal operation identity/state mismatch");
        }
        private_events::enqueue_delivery(
            tx,
            &DeliveryIntentInput {
                id: operation,
                tenant_id: plan.tenant_id,
                destination_id: PAYROLL,
                destination_version: 1,
                source_kind: kind.into(),
                resource_id: operation,
                event_type: event.into(),
                payload: json!({"operationId":operation}),
                occurred_at: timestamp(row, "available_at")?,
            },
        )
        .await?;
    }
    let resource = if e.legacy_table == "outbox_event" {
        e.legacy_id
    } else {
        field_uuid(row, "operation_id")?
    };
    let receipts=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT intent_id,destination_id,intent_hash,destination_version,source_generation FROM private_delivery_receipt WHERE tenant_id=$1 AND resource_id=$2 AND source_kind IN ('webhook','finance_confirmation','finance_cancellation') ORDER BY destination_id FOR UPDATE",[plan.tenant_id.into(),resource.into()])).await?;
    if receipts.len() != e.expected_destinations.len() {
        bail!("Source fanout differs from reviewed expected destination set");
    }
    let mut owned = Vec::new();
    for r in receipts {
        let mut acknowledged = false;
        let destination: Uuid = r.try_get("", "destination_id")?;
        let intent: Uuid = r.try_get("", "intent_id")?;
        let version: i64 = r.try_get("", "destination_version")?;
        if r.try_get::<i64>("", "source_generation")? != plan.source_generation
            || !e
                .expected_destinations
                .iter()
                .any(|d| d.destination_id == destination && d.destination_version == version)
        {
            bail!("Destination version/source generation changed");
        }
        if e.legacy_table == "outbox_event" {
            let logs=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT to_jsonb(t) AS row FROM webhook_delivery_log t WHERE tenant_id=$1 AND webhook_subscription_id=$2 AND payload_json->>'outbox_id'=$3 AND is_success=true AND http_status BETWEEN 200 AND 299 AND payload_json->'payload'=(SELECT payload FROM outbox_event WHERE tenant_id=$1 AND id=$4) ORDER BY created_at,id LIMIT 1001 FOR SHARE",[plan.tenant_id.into(),destination.into(),e.legacy_id.to_string().into(),e.legacy_id.into()])).await?;
            if logs.len() > 1000 {
                bail!("Acknowledgement reconciliation exceeds bounded review");
            }
            for log in logs {
                let log: Value = log.try_get("", "row")?;
                if ack_matches(&log, row, plan.tenant_id, destination) {
                    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET status='DELIVERED',accepted_at=$4,acknowledgement_id=$5,outcome_code='legacy_correlated_v1_http_2xx',updated_at=clock_timestamp() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND status='PENDING'",[plan.tenant_id.into(),intent.into(),destination.into(),timestamp(&log,"created_at")?.into(),field_uuid(&log,"id")?.into()])).await?;
                    acknowledged = true;
                    break;
                }
            }
        }
        if uncertain && !acknowledged && !e.authorize_retry_uncertain {
            bail!("Uncertain destination outcome requires explicit operator retry resolution");
        }
        owned.push((destination, intent, r.try_get("", "intent_hash")?));
    }
    Ok(owned)
}
pub async fn run(
    db: &DatabaseConnection,
    bound: Uuid,
    path: &str,
    execute: bool,
) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("Migration plan exceeds 64KiB");
    }
    let plan: Plan = serde_json::from_slice(&bytes)?;
    validate(&plan, bound)?;
    let tx = db.begin().await?;
    tx.query_one(Statement::from_string(DbBackend::Postgres,"SELECT set_config('TimeZone','UTC',true),set_config('DateStyle','ISO,YMD',true),set_config('extra_float_digits','3',true),set_config('bytea_output','hex',true)".to_string())).await?;
    let a=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT producer_generation,state,writers_fenced FROM private_source_cutover WHERE tenant_id=$1 FOR UPDATE",[bound.into()])).await?.context("Source authority missing")?;
    if a.try_get::<i64>("", "producer_generation")? != plan.source_generation
        || a.try_get::<String>("", "state")? != "FENCED"
        || !a.try_get::<bool>("", "writers_fenced")?
    {
        bail!("Current legacy generation must be fenced before migration");
    }
    for e in &plan.entries {
        let (row, hash) = source_row(&tx, &e.legacy_table, e.legacy_id, bound).await?;
        if hash != e.source_row_sha256 {
            bail!("Reviewed source row changed");
        }
        let manifest=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id FROM private_legacy_export WHERE id=$1 AND tenant_id=$2 AND source_table=$3 AND producer_generation=$4 AND revoked_at IS NULL FOR SHARE",[e.manifest_id.into(),bound.into(),e.legacy_table.clone().into(),plan.source_generation.into()])).await?;
        if manifest.is_none() {
            bail!("Full raw export manifest binding missing");
        }
        let owners = match e.classification.as_str() {
            "HISTORY_ONLY" => {
                if !history_only(&e.legacy_table, &row) {
                    bail!("Pending/ambiguous identity cannot be inferred historical");
                }
                Vec::new()
            }
            "ACKNOWLEDGED" => {
                if e.legacy_table != "webhook_delivery_log" {
                    bail!("Acknowledgement resolution requires an exact webhook log");
                }
                ensure_ack_log(&tx, &row, bound).await?;
                Vec::new()
            }
            "TRANSFERRED" => transfer(&tx, &plan, e, &row).await?,
            "RECONCILED_NONEXECUTABLE" => Vec::new(),
            _ => unreachable!(),
        };
        let resolution = private_events::crypto::source_hash(&serde_json::to_vec(&(
            plan.tenant_id,
            plan.source_generation,
            &plan.accepted_by,
            e,
        ))?);
        let destinations: Vec<Uuid> = e
            .expected_destinations
            .iter()
            .map(|d| d.destination_id)
            .collect();
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_pending_migration(tenant_id,legacy_table,legacy_id,manifest_id,source_generation,source_row_sha256,classification,expected_destination_ids,resolution_sha256,accepted_by,accepted_at) VALUES($1,$2,$3,$4,$5,$6,$7,ARRAY(SELECT value::uuid FROM jsonb_array_elements_text($8::jsonb)),$9,$10,clock_timestamp()) ON CONFLICT(tenant_id,legacy_table,legacy_id) DO NOTHING",[bound.into(),e.legacy_table.clone().into(),e.legacy_id.into(),e.manifest_id.into(),plan.source_generation.into(),hash.into(),e.classification.clone().into(),serde_json::to_value(destinations)?.into(),resolution.clone().into(),plan.accepted_by.clone().into()])).await?;
        let prior=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT resolution_sha256 FROM private_pending_migration WHERE tenant_id=$1 AND legacy_table=$2 AND legacy_id=$3",[bound.into(),e.legacy_table.clone().into(),e.legacy_id.into()])).await?.context("Migration receipt missing")?;
        if prior.try_get::<String>("", "resolution_sha256")? != resolution {
            bail!("Conflicting prior operator resolution");
        }
        for (destination, intent, hash) in owners {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_pending_destination_owner(tenant_id,legacy_table,legacy_id,destination_id,intent_id,intent_hash) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,legacy_table,legacy_id,destination_id) DO NOTHING",[bound.into(),e.legacy_table.clone().into(),e.legacy_id.into(),destination.into(),intent.into(),hash.into()])).await?;
        }
    }
    if execute {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    tracing::info!(
        entries = plan.entries.len(),
        execute,
        "source legacy delivery migration reviewed"
    );
    Ok(())
}
pub async fn verify_ownership(db: &DatabaseConnection, bound: Uuid) -> anyhow::Result<()> {
    let tx = db.begin().await?;
    let a=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT producer_generation,state,writers_fenced FROM private_source_cutover WHERE tenant_id=$1 FOR UPDATE",[bound.into()])).await?.context("Source authority missing")?;
    if a.try_get::<String>("", "state")? != "FENCED" || !a.try_get::<bool>("", "writers_fenced")? {
        bail!("Source fence required");
    }
    let generation: i64 = a.try_get("", "producer_generation")?;
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT private_assert_pending_migration($1,$2)",
        [bound.into(), generation.into()],
    ))
    .await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_source_cutover SET pending_ownership_complete=true,updated_at=clock_timestamp() WHERE tenant_id=$1 AND producer_generation=$2",[bound.into(),generation.into()])).await?;
    tx.commit().await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires explicit disposable PRIVATE_LEGACY_TEST_DATABASE_URL"]
    async fn postgres_source_pending_fanout_preserves_acknowledged_destinations_and_gates_ownership(
    ) {
        use sea_orm::{ConnectOptions, Database};
        let url = std::env::var("PRIVATE_LEGACY_TEST_DATABASE_URL")
            .expect("isolated source fixture URL required");
        assert!(
            url.starts_with("postgresql://private_fixture:private_fixture@127.0.0.1:")
                && url.ends_with("/private_retirement_fixture"),
            "refusing a non-fixture database"
        );
        let schema = format!("pending_test_{}", Uuid::new_v4().simple());
        let mut options = ConnectOptions::new(url);
        options
            .max_connections(1)
            .sqlx_logging(false)
            .set_schema_search_path(format!("{schema},public"));
        let db = Database::connect(options).await.unwrap();
        db.execute_unprepared(&format!(r#"CREATE SCHEMA {schema};
CREATE TABLE {schema}.private_source_cutover(tenant_id uuid PRIMARY KEY,producer_generation bigint,state text,writers_fenced boolean,pending_ownership_complete boolean DEFAULT false,updated_at timestamptz);
CREATE TABLE {schema}.private_legacy_export(id uuid PRIMARY KEY,tenant_id uuid,source_table text,producer_generation bigint,revoked_at timestamptz);
CREATE TABLE {schema}.private_pending_migration(tenant_id uuid,legacy_table text,legacy_id uuid,manifest_id uuid,source_generation bigint,source_row_sha256 char(64),classification text,expected_destination_ids uuid[],resolution_sha256 char(64),accepted_by text,accepted_at timestamptz,PRIMARY KEY(tenant_id,legacy_table,legacy_id));
CREATE TABLE {schema}.private_pending_destination_owner(tenant_id uuid,legacy_table text,legacy_id uuid,destination_id uuid,intent_id uuid,intent_hash char(64),PRIMARY KEY(tenant_id,legacy_table,legacy_id,destination_id));
CREATE TABLE {schema}.private_delivery_receipt(tenant_id uuid,intent_id uuid,destination_id uuid,source_kind text,resource_id uuid,intent_hash char(64),destination_version bigint,status text,source_generation bigint,created_at timestamptz,updated_at timestamptz,accepted_at timestamptz,acknowledgement_id uuid,broker_owned_at timestamptz,broker_owner_id uuid,broker_receipt_sha256 char(64),outcome_code text,PRIMARY KEY(tenant_id,intent_id,destination_id));
CREATE TABLE {schema}.company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid,organization_id uuid,event_type text,aggregate_type text,aggregate_id uuid,aggregate_version bigint,event_envelope jsonb,event_stream text,created_at timestamptz);
CREATE TABLE {schema}.outbox_event(id uuid PRIMARY KEY,tenant_id uuid,aggregate_type text,aggregate_id uuid,event_type text,payload jsonb,status text,retry_count integer,created_at timestamptz);
CREATE TABLE {schema}.webhook_subscription(id uuid PRIMARY KEY,tenant_id uuid,event_name text,is_active boolean,config_version bigint,created_at timestamptz);
CREATE TABLE {schema}.webhook_delivery_log(id uuid PRIMARY KEY,tenant_id uuid,webhook_subscription_id uuid,payload_json jsonb,http_status integer,is_success boolean,created_at timestamptz);
CREATE TABLE {schema}.employee_finance_outbox(id uuid PRIMARY KEY,tenant_id uuid,operation_id uuid,event_type text,payload jsonb,attempts integer,available_at timestamptz,delivered_at timestamptz);
CREATE TABLE {schema}.employee_exit_operation(id uuid PRIMARY KEY,tenant_id uuid,state text);
"#)).await.unwrap();
        let migration=include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../../hrms-database/changelog/migrations/0125_private_event_source/private_event_source.xml"));
        let begin = migration
            .find("CREATE FUNCTION \"${schema}\".private_assert_pending_migration(")
            .unwrap();
        let function = &migration[begin..];
        let end = function.find("END $fn$;").unwrap() + "END $fn$;".len();
        db.execute_unprepared(&function[..end].replace("${schema}", &schema))
            .await
            .unwrap();
        let tenant = Uuid::new_v4();
        let event = Uuid::new_v4();
        let resource = Uuid::new_v4();
        let dest1 = Uuid::new_v4();
        let dest2 = Uuid::new_v4();
        let log = Uuid::new_v4();
        let finance = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let at: DateTime<Utc> = "2026-10-19T12:13:14.123456Z".parse().unwrap();
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO private_source_cutover(tenant_id,producer_generation,state,writers_fenced) VALUES($1,1,'FENCED',true)",[tenant.into()])).await.unwrap();
        for dest in [dest1, dest2] {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO webhook_subscription VALUES($1,$2,'expense.approved',true,1,$3)",
                [dest.into(), tenant.into(), at.into()],
            ))
            .await
            .unwrap();
        }
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO outbox_event VALUES($1,$2,'expense',$3,'expense.approved',$4,'FAILED',1,$5)",[event.into(),tenant.into(),resource.into(),json!({"amount":17}).into(),at.into()])).await.unwrap();
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO webhook_delivery_log VALUES($1,$2,$3,$4,204,true,$5)",[log.into(),tenant.into(),dest1.into(),json!({"schema_version":1,"outbox_id":event,"tenant_id":tenant,"aggregate_type":"expense","aggregate_id":resource,"event_type":"expense.approved","payload":{"amount":17},"created_at":at}).into(),at.into()])).await.unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO employee_exit_operation VALUES($1,$2,'COMMITTED')",
            [operation.into(), tenant.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO employee_finance_outbox VALUES($1,$2,$3,'EXIT_APPROVAL_COMMITTED',$4,0,$5,NULL)",[finance.into(),tenant.into(),operation.into(),json!({"operationId":operation,"old":"retained only in raw history"}).into(),at.into()])).await.unwrap();
        let keyfile =
            std::env::temp_dir().join(format!("private-migration-key-{}.json", Uuid::new_v4()));
        std::fs::write(&keyfile,serde_json::to_vec(&json!({"schema_version":1,"keys":[{"tenant_id":tenant,"purpose":"delivery","key_id":"fixture","key_b64":"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=","active":true}]})).unwrap()).unwrap();
        let old_key = std::env::var_os("KABIPAY_PRIVATE_EVENT_KEYS_FILE");
        let old_skip = std::env::var_os("OUTBOX_SKIP_TENANT_WEBHOOKS");
        let old_platform = std::env::var_os("OUTBOX_WEBHOOK_URL");
        std::env::set_var("KABIPAY_PRIVATE_EVENT_KEYS_FILE", &keyfile);
        std::env::remove_var("OUTBOX_SKIP_TENANT_WEBHOOKS");
        std::env::remove_var("OUTBOX_WEBHOOK_URL");
        let mut entries = Vec::new();
        for (table, id, classification, destinations) in [
            (
                "outbox_event",
                event,
                "TRANSFERRED",
                json!([{"destination_id":dest1,"destination_version":1},{"destination_id":dest2,"destination_version":1}]),
            ),
            ("webhook_delivery_log", log, "ACKNOWLEDGED", json!([])),
            (
                "employee_finance_outbox",
                finance,
                "TRANSFERRED",
                json!([{"destination_id":PAYROLL,"destination_version":1}]),
            ),
        ] {
            let manifest = Uuid::new_v4();
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO private_legacy_export VALUES($1,$2,$3,1,NULL)",
                [manifest.into(), tenant.into(), table.into()],
            ))
            .await
            .unwrap();
            let tx = db.begin().await.unwrap();
            let (_, hash) = source_row(&tx, table, id, tenant).await.unwrap();
            tx.rollback().await.unwrap();
            entries.push(json!({"legacy_table":table,"legacy_id":id,"manifest_id":manifest,"source_row_sha256":hash,"classification":classification,"reason":"operator reviewed exact source destination and prior acknowledgement","authorize_retry_uncertain":true,"expected_destinations":destinations}));
        }
        let plan = json!({"version":1,"tenant_id":tenant,"source_generation":1,"accepted_by":"isolated-source-operator","entries":entries});
        let path =
            std::env::temp_dir().join(format!("private-migration-plan-{}.json", Uuid::new_v4()));
        let pathstr = path.to_str().unwrap();
        let mut unresolved = plan.clone();
        unresolved["entries"][0]["authorize_retry_uncertain"] = json!(false);
        std::fs::write(&path, serde_json::to_vec(&unresolved).unwrap()).unwrap();
        assert!(run(&db, tenant, pathstr, true).await.is_err());
        std::fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        run(&db, tenant, pathstr, false).await.unwrap();
        let count = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS n FROM private_delivery_receipt".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count.try_get::<i64>("", "n").unwrap(), 0);
        run(&db, tenant, pathstr, true).await.unwrap();
        run(&db, tenant, pathstr, true).await.unwrap();
        let ack=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT status,acknowledgement_id FROM private_delivery_receipt WHERE tenant_id=$1 AND destination_id=$2",[tenant.into(),dest1.into()])).await.unwrap().unwrap();
        assert_eq!(ack.try_get::<String>("", "status").unwrap(), "DELIVERED");
        assert_eq!(ack.try_get::<Uuid>("", "acknowledgement_id").unwrap(), log);
        assert!(verify_ownership(&db, tenant).await.is_err());
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE private_delivery_receipt SET broker_owned_at=clock_timestamp(),broker_owner_id=$1,broker_receipt_sha256=repeat('b',64) WHERE accepted_at IS NULL",[Uuid::new_v4().into()])).await.unwrap();
        verify_ownership(&db, tenant).await.unwrap();
        let flag = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pending_ownership_complete FROM private_source_cutover WHERE tenant_id=$1",
                [tenant.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(flag
            .try_get::<bool>("", "pending_ownership_complete")
            .unwrap());
        let mut conflict = plan;
        conflict["entries"][0]["reason"] = json!("conflicting new reviewed resolution");
        std::fs::write(&path, serde_json::to_vec(&conflict).unwrap()).unwrap();
        assert!(run(&db, tenant, pathstr, true).await.is_err());
        db.execute_unprepared("UPDATE private_source_cutover SET producer_generation=2")
            .await
            .unwrap();
        assert!(verify_ownership(&db, tenant).await.is_err());
        for (name, old) in [
            ("KABIPAY_PRIVATE_EVENT_KEYS_FILE", old_key),
            ("OUTBOX_SKIP_TENANT_WEBHOOKS", old_skip),
            ("OUTBOX_WEBHOOK_URL", old_platform),
        ] {
            match old {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(keyfile).unwrap();
        db.execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
        db.close().await.unwrap();
    }
    #[test]
    fn terminal_and_pending_classification_are_separate() {
        assert!(history_only("outbox_event", &json!({"status":"PROCESSED"})));
        assert!(!history_only(
            "outbox_event",
            &json!({"status":"PROCESSING"})
        ));
        assert!(history_only(
            "employee_finance_outbox",
            &json!({"delivered_at":"2026-10-19T00:00:00Z"})
        ));
        assert!(!history_only(
            "webhook_delivery_log",
            &json!({"delivered_at":"2026-10-19T00:00:00Z","is_success":true})
        ));
    }
    #[test]
    fn exact_v1_source_correlation_is_required_for_acknowledgement() {
        let tenant = Uuid::from_u128(1);
        let destination = Uuid::from_u128(2);
        let event = json!({"id":Uuid::from_u128(3),"tenant_id":tenant,"aggregate_id":Uuid::from_u128(4),"aggregate_type":"expense","event_type":"expense.approved","payload":{"amount":17},"created_at":"2026-10-19T10:20:30.123456Z"});
        let payload = json!({"schema_version":1,"outbox_id":event["id"],"tenant_id":tenant,"aggregate_id":event["aggregate_id"],"aggregate_type":event["aggregate_type"],"event_type":event["event_type"],"payload":event["payload"],"created_at":"2026-10-19T10:20:30.123456+00:00"});
        let log = json!({"tenant_id":tenant,"webhook_subscription_id":destination,"is_success":true,"http_status":204,"payload_json":payload});
        assert!(ack_matches(&log, &event, tenant, destination));
        for key in ["outbox_id", "tenant_id", "aggregate_id"] {
            let mut changed = log.clone();
            changed["payload_json"][key] = json!(Uuid::new_v4());
            assert!(!ack_matches(&changed, &event, tenant, destination));
        }
        for status in [0, 199, 300, 500] {
            let mut changed = log.clone();
            changed["http_status"] = json!(status);
            assert!(!ack_matches(&changed, &event, tenant, destination));
        }
        let mut changed = log.clone();
        changed["payload_json"]["created_at"] = json!("2026-10-19T10:20:30.123457Z");
        assert!(!ack_matches(&changed, &event, tenant, destination));
        let mut changed = log;
        changed["payload_json"]["schema_version"] = json!(2);
        assert!(!ack_matches(&changed, &event, tenant, destination));
    }
    #[test]
    fn operator_plan_cannot_omit_destination_identity_or_expand_scope() {
        let tenant = Uuid::from_u128(1);
        let value = json!({"version":1,"tenant_id":tenant,"source_generation":1,"accepted_by":"operator-123","entries":[{"legacy_table":"outbox_event","legacy_id":Uuid::from_u128(2),"manifest_id":Uuid::from_u128(3),"source_row_sha256":"a".repeat(64),"classification":"TRANSFERRED","reason":"reviewed exact source subscription set","expected_destinations":[{"destination_id":Uuid::from_u128(4),"destination_version":1}]}]});
        let plan: Plan = serde_json::from_value(value.clone()).unwrap();
        assert!(validate(&plan, tenant).is_ok());
        assert!(validate(&plan, Uuid::new_v4()).is_err());
        let mut missing = value.clone();
        missing["entries"][0]["expected_destinations"] = json!([]);
        assert!(validate(&serde_json::from_value(missing).unwrap(), tenant).is_err());
        let mut duplicate = value;
        duplicate["entries"][0]["expected_destinations"] = json!([{"destination_id":Uuid::from_u128(4),"destination_version":1},{"destination_id":Uuid::from_u128(4),"destination_version":1}]);
        assert!(validate(&serde_json::from_value(duplicate).unwrap(), tenant).is_err());
    }
}
