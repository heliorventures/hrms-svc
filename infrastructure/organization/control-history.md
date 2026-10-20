# Control-plane CDC and retained security history

Current installation/credential/role commands: [independent release procedure](../../../hrms-documentation/docs/deployment/organization-infrastructure.md). Application consumers remain in svc; broker/Connect and database setup are independently owned.

This is a separate trust domain from company fact ingestion. It captures the fixed metadata database's `kabipay_ops.organization_outbox` (migration0113), including principal/login-link rows whose organization ID is NULL. The consumer accepts **only** the configured metadata topic and native Debezium source database/schema/table/connector identity. It has Kafka and ClickHouse credentials, no PostgreSQL connection or tenant credentials. The dedicated control Connect instance has only the fixed metadata CDC role; it is not attached to company source networks or company secret mounts.

## Deployment contract

1. Apply [control-history.sql](../../../hrms-clickhouse/migrations/0002-control-history.sql) through a DBA and provision separate `organization_control_worker`, `organization_control_reader` and operator identities. Worker gets SELECT/INSERT only on control history and current checkpoint; no company history, DDL, DELETE or business DB privileges. App reader gets only control history with the nonnull row policy shown in SQL; no `security_audit` grant. Operator audit access is separate and logged. Root admin APIs must additionally check current orgManage and exact `organization_id` equality, bounded dates/rows and cursor. NULL-organization security events are never exposed through organization admin APIs.
2. Provision the dedicated metadata LOGIN REPLICATION role, TLS certificate and private pg_hba restriction, then apply [control-cdc-grants.template.sql](../../../hrms-database/provisioning/organization/control-cdc-grants.template.sql) as DBA. Replication privilege is sensitive: restrict the exact database, publication and network; no superuser/business writes or SELECT on private principal/login-link/tenant/config tables. Publication is only the outbox. Monitor WAL retained bytes, slot invalidation/lag, connector status and sink/broker lag.
3. Merge [control-history.compose.template.yaml](../../../hrms-kafka/deploy/control-connect.compose.yaml) with the existing private composition. It adds a separate Connect instance and consumer with **no public ports**. Provision `secrets/control-connect` and `secrets/control-worker` separately. Submit the reviewed connector over the private management network; never expose the Connect REST API publicly. Its fixed binding must match `control-binding.example.json` values exactly. Native row provenance is preserved; no company event router or per-company binding is reused.
4. Precreate topics: `organization.control.metadata` (**exactly one partition**), `organization.control.quarantine`, `__debezium-heartbeat.organization_control_metadata` and `organization.control.connect.{config,offset,status}`. Connect config topic needs one partition/compaction; offsets/status use reviewed compaction and replication. Business topic retention is explicitly seven days (604800000ms) and bounded bytes sized against the maximum outage/backfill rate; protected hash-only quarantine retention is30days. Heartbeat retention is one day. Choose replication factor/min.insync.replicas for the actual broker count; reference single-host composition has no host-failure availability.
5. ACLs: metadata connector principal WRITE/DESCRIBE only the fixed control topic and heartbeat plus its own Connect internal topics/group; worker READ only control topic/group and WRITE only control quarantine. Company producer/consumer principals get no control topic/audit access. Worker verifies the topic has one partition at startup. Do not increase partitions or run two writer groups: current conflict detection depends on one ordered writer. A scaled design requires an explicit durable conflict fence. Never replay through an independent concurrent writer group.

**Heartbeat topic is mandatory when broker auto-creation is disabled.** Actual synthetic acceptance demonstrated that an absent `__debezium-heartbeat.<prefix>` topic blocks streaming after a successful snapshot. This applies to the existing company connector template too: explicitly provision its own fixed heartbeat topic/ACL. A connector heartbeat advances connector/WAL observations; it does not prove all historical rows or every company partition are covered.

## Validation, idempotency and incomplete capture

The strict schema accepts source ops `c`/`r` only (append/snapshot), schema_version1, UUID identifiers, bounded typed changes and verified actor `{principal_id,source_tenant_id,source_user_id,issuer:'kabipay-client'}` matching row actor/tenant. Unexpected columns/event types/operations/oversized input, unsafe integer versions or provenance mismatches are hash-only quarantined. No raw malformed row, private payload, actor name, exception body or secret is logged or sent to quarantine. Safe-integer overflow is explicit quarantine, never silent rounded storage. Before enabling an event producer change, deploy and check its corresponding validator schema.

Current types: identity.registered/revoked (NULL organization only), organization.bootstrapped/configuration_changed, relationship.proposed/manager_changed/ended/installed, grant.installed, membership.installed, directory.synchronized and command.cancelled. Configuration requests whitelist membership lifecycle, role capabilities/scopes and explicit company IDs; arbitrary JSON/private bodies are rejected. Command cancellation retains only command/resource/membership/operation/version references, not command payload.

`aggregate_version` is the monotonic **outbox sequence** from root `store::event`, not the configuration revision. `payload.configuration_version` is separately retained as `configuration_version`. Proposed and installed relationship facts may both have configuration revision1, but their outbox sequence must be1 then2. The actual SQL fixture extracts and executes root's writer SQL plus real0113/0114 sequence DDL to prove this contract. Do not introduce another outbox writer that bypasses this sequence.

ClickHouse ReplacingMergeTree deduplicates event-ID replays; every query uses FINAL. Before insert, same event ID or same metadata source/aggregate/sequence with different content or identity is quarantined; accepted facts are never overwritten. Kafka offsets advance only after synchronous history INSERT and checkpoint acknowledgement. Uncertain CH acknowledgement safely replays. Malformed/conflicting input advances only after durable hash-only quarantine and a sticky QUARANTINED checkpoint. A failed sink or quarantine acknowledgement does not commit the broker offset. Recovery reuses the same group. Quarantine reconciliation requires reviewed reprocessing plus an explicit capture attestation; a subsequent valid event never clears the partial state automatically.

`control_capture_checkpoint` is current observed progress, not a capture acceptance manifest. `security_audit` is an operator view over retained control facts, separated from diagnostics. Table retention is explicitly seven years by event time (review regulatory/business policy before deployment); backup/archive must preserve the same access controls and hashes. It does not assert historical completeness. Initial CDC captures only rows still present in the outbox; earlier actions without rows require a reconciled, purpose-limited historical export with provenance. Root APIs label activity NOT_ATTESTED until a separately accepted capture/backfill/restore manifest exists.

## Read contract and operations

Admin query shape (bind all parameters, never interpolate them):

```sql
SELECT event_id,event_type,organization_id,source_tenant_id,actor_principal_id,
 aggregate_id,toString(aggregate_version) AS aggregate_version,
 toString(configuration_version) AS configuration_version,
 formatDateTime(occurred_at,'%Y-%m-%dT%H:%i:%S.%fZ','UTC') AS occurred_at,
 actor
FROM organization.control_history FINAL
WHERE organization_id={organization:UUID}
 AND occurred_at>={from:DateTime64(3,'UTC')}
 AND occurred_at<={to:DateTime64(3,'UTC')}
 AND event_id>{after:UUID}
ORDER BY event_id LIMIT {limit:UInt16}
FORMAT JSONEachRow
```

Omit `change` from app responses unless the full per-kind whitelist is implemented at the read boundary too. Full typed changes remain protected for operations. Use an operator-only endpoint for NULL-organization identity/security history; never infer that organization admin controls global principals. The audit view explicitly uses SQL SECURITY INVOKER, so only the separately provisioned global operator receives SELECT on both that view and its source control table. No definer bypass or app audit-view grant is used; see [control-history-grants.sql](../../../hrms-clickhouse/grants/0004-control-history.sql).

Health: monitor topic end offset versus committed group offset, sticky quarantine state/first gap, connector task status, source slot/WAL lag, CH insert errors/storage/TTL/backups and current checkpoint age. No events is not proof of current/captured history. On restore, stop the single writer, restore CH from an independently verified backup/archive, replay the original topic/group range, compare event IDs/content hashes and outbox sequence coverage, then attest the exact range. Topic retention exhaustion or slot invalidation requires a reconciled resnapshot; never reset offsets and label the result complete.

**No control-outbox cleanup is enabled.** The existing company outbox planner cannot target this table. Preserve all control rows until an independently reviewed control manifest proves exact row count/IDs/hashes, acknowledged CDC LSN, no quarantines/gaps, history retention and tested backup restore. Never delete source business command receipts or legacy audit history merely because CDC is running.

## Executed acceptance

From `hrms-svc/integrations/organization-events`, run the focused `npm test` and explicitly authorized `ORGANIZATION_SYNTHETIC_ACCEPTANCE=true node test/control-cdc-acceptance.mjs`. The latter creates a random dedicated DB/role/connector/slot in the existing isolated stack, applies actual0113/0114 sequence DDL, executes root writer SQL, then removes only those new source resources. Synthetic Kafka/CH evidence is retained for inspection. It starts the real consumer executable, verifies NULL-org principal snapshot plus live proposed/installed events, rollback invisibility,1/2 sequence versus1/1 configuration revisions, sink failure holding broker offset3, replay recovery, duplicate/conflict handling and hash-only quarantine. It does not establish production TLS/ACL, external backup/archive/restore, complete old history or deployed availability.

Primary references: [Debezium PostgreSQL connector](https://debezium.io/documentation/reference/3.2/connectors/postgresql.html), [Kafka Connect RegexRouter](https://kafka.apache.org/40/kafka-connect/developer-guide#connect_transforms), [ClickHouse ReplacingMergeTree](https://clickhouse.com/docs/engines/table-engines/mergetree-family/replacingmergetree).
