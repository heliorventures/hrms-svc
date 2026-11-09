//! Source-authorized egress. Shared delivery callers supply references and proofs only.
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use kabipay_common::{
    finance_contract::{FinanceAction, FinanceAuthorizationState, HrOperationState, ServiceCaller},
    finance_transport,
    private_events::{
        self,
        crypto::{source_hash, Envelope},
        DeliveryIntentInput,
    },
    KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{io::Read, time::Duration};
use uuid::Uuid;

pub const PAYROLL_DESTINATION: Uuid = Uuid::from_u128(0x00000000000040008000000000000002);
const PLATFORM_DESTINATION: Uuid = Uuid::from_u128(0x00000000000040008000000000000001);

fn rejected() -> KabiPayError {
    KabiPayError::Forbidden("Private delivery authorization rejected".into())
}
fn unavailable() -> KabiPayError {
    KabiPayError::Conflict("Private delivery requires source reconciliation".into())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRequest {
    pub version: u8,
    pub tenant_id: Uuid,
    pub intent_id: Uuid,
    pub destination_id: Uuid,
    pub source_generation: i64,
    pub destination_version: i64,
    pub intent_hash: String,
    pub lease_generation: i64,
    pub request_id: Uuid,
    pub issued_at: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDeliveryRequest {
    pub request: DeliveryRequest,
    pub signature: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryResponse {
    pub version: u8,
    pub tenant_id: Uuid,
    pub intent_id: Uuid,
    pub destination_id: Uuid,
    pub source_generation: i64,
    pub destination_version: i64,
    pub intent_hash: String,
    pub lease_generation: i64,
    pub request_id: Uuid,
    pub issued_at: i64,
    pub status: String,
    pub outcome_code: String,
    pub acknowledgement_id: Option<Uuid>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDeliveryResponse {
    pub response: DeliveryResponse,
    pub signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterConfig {
    schema_version: u8,
    bindings: Vec<Binding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    tenant_id: Uuid,
    source_generation: i64,
    key_b64: String,
    allowed_webhook_hosts: Vec<String>,
    webhook_keys: Vec<WebhookKey>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookKey {
    key_id: String,
    key_b64: String,
}

fn key(encoded: &str) -> KabiPayResult<Vec<u8>> {
    let bytes = STANDARD.decode(encoded).map_err(|_| rejected())?;
    if !(32..=128).contains(&bytes.len()) || STANDARD.encode(&bytes) != encoded {
        return Err(rejected());
    }
    Ok(bytes)
}
fn configured_binding(tenant: Uuid) -> KabiPayResult<Binding> {
    let path = kabipay_common::db::company_setting(tenant, "KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(65537).read_to_end(&mut bytes))
        .map_err(|_| unavailable())?;
    binding_from_bytes(&bytes, tenant)
}
fn binding_from_bytes(bytes: &[u8], tenant: Uuid) -> KabiPayResult<Binding> {
    if bytes.len() > 65536 {
        return Err(unavailable());
    }
    let config: AdapterConfig = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if config.schema_version != 1 || config.bindings.len() > 256 {
        return Err(unavailable());
    }
    let mut seen = std::collections::HashSet::new();
    for binding in &config.bindings {
        if binding.tenant_id.is_nil()
            || binding.source_generation < 1
            || binding.source_generation > 9_007_199_254_740_991
            || !seen.insert(binding.tenant_id)
            || binding.allowed_webhook_hosts.len() > 128
            || binding.webhook_keys.len() > 128
        {
            return Err(unavailable());
        }
        key(&binding.key_b64)?;
        let mut refs = std::collections::HashSet::new();
        for entry in &binding.webhook_keys {
            if entry.key_id.is_empty() || entry.key_id.len() > 160 || !refs.insert(&entry.key_id) {
                return Err(unavailable());
            }
            key(&entry.key_b64)?;
        }
    }
    config
        .bindings
        .into_iter()
        .find(|binding| binding.tenant_id == tenant)
        .ok_or_else(rejected)
}
fn mac<T: Serialize>(domain: &[u8], body: &T, secret: &[u8]) -> KabiPayResult<Hmac<Sha256>> {
    if secret.len() < 32 {
        return Err(rejected());
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).map_err(|_| rejected())?;
    mac.update(domain);
    mac.update(&serde_json::to_vec(body)?);
    Ok(mac)
}
pub fn verify_request(
    envelope: &SignedDeliveryRequest,
    tenant: Uuid,
    generation: i64,
    secret: &[u8],
    now: i64,
) -> KabiPayResult<()> {
    let request = &envelope.request;
    if request.version != 1
        || request.tenant_id != tenant
        || request.source_generation != generation
        || [
            request.tenant_id,
            request.intent_id,
            request.destination_id,
            request.request_id,
        ]
        .iter()
        .any(Uuid::is_nil)
        || request.destination_version < 1
        || request.destination_version > 9_007_199_254_740_991
        || request.lease_generation < 0
        || request.lease_generation > 9_007_199_254_740_991
        || request.intent_hash.len() != 64
        || !request
            .intent_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || request.issued_at < now.saturating_sub(60)
        || request.issued_at > now.saturating_add(5)
    {
        return Err(rejected());
    }
    let signature = URL_SAFE_NO_PAD
        .decode(&envelope.signature)
        .map_err(|_| rejected())?;
    if URL_SAFE_NO_PAD.encode(&signature) != envelope.signature {
        return Err(rejected());
    }
    mac(b"kabipay-private-delivery-request-v1\0", request, secret)?
        .verify_slice(&signature)
        .map_err(|_| rejected())
}
fn signed_response(
    request: &DeliveryRequest,
    generation: i64,
    status: &str,
    code: &str,
    ack: Option<Uuid>,
    secret: &[u8],
) -> KabiPayResult<SignedDeliveryResponse> {
    let response = DeliveryResponse {
        version: 1,
        tenant_id: request.tenant_id,
        intent_id: request.intent_id,
        destination_id: request.destination_id,
        source_generation: request.source_generation,
        destination_version: request.destination_version,
        intent_hash: request.intent_hash.clone(),
        lease_generation: generation,
        request_id: request.request_id,
        issued_at: Utc::now().timestamp(),
        status: status.into(),
        outcome_code: code.into(),
        acknowledgement_id: ack,
    };
    let signature = URL_SAFE_NO_PAD.encode(
        mac(b"kabipay-private-delivery-response-v1\0", &response, secret)?
            .finalize()
            .into_bytes(),
    );
    Ok(SignedDeliveryResponse {
        response,
        signature,
    })
}

pub fn authorize(envelope: &SignedDeliveryRequest) -> KabiPayResult<()> {
    let binding = configured_binding(envelope.request.tenant_id)?;
    verify_request(
        envelope,
        binding.tenant_id,
        binding.source_generation,
        &key(&binding.key_b64)?,
        Utc::now().timestamp(),
    )
}

pub(super) fn ownership_secret(tenant: Uuid) -> KabiPayResult<(i64, Vec<u8>)> {
    let binding = configured_binding(tenant)?;
    Ok((binding.source_generation, key(&binding.key_b64)?))
}

async fn load_intent<C: ConnectionTrait>(
    db: &C,
    request: &DeliveryRequest,
) -> KabiPayResult<DeliveryIntentInput> {
    let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT event_envelope FROM company_event_outbox WHERE source_tenant_id=$1 AND event_stream='delivery' AND event_envelope->>'record_id'=$2 ORDER BY (event_envelope->>'frame_index')::int LIMIT 65",
        [request.tenant_id.into(), request.intent_id.to_string().into()])).await?;
    if rows.is_empty() || rows.len() > 64 {
        return Err(unavailable());
    }
    let frames: Vec<Envelope> = rows
        .into_iter()
        .map(|row| {
            let value: serde_json::Value = row.try_get("", "event_envelope")?;
            serde_json::from_value(value).map_err(|_| rejected())
        })
        .collect::<KabiPayResult<_>>()?;
    if frames
        .iter()
        .any(|frame| frame.source_generation != request.source_generation)
    {
        return Err(rejected());
    }
    let bytes = private_events::configured_company_keyring(request.tenant_id)?.decrypt_frames(
        &frames,
        request.tenant_id,
        "delivery",
    )?;
    if source_hash(&bytes) != request.intent_hash {
        return Err(rejected());
    }
    let intent: DeliveryIntentInput = serde_json::from_slice(&bytes).map_err(|_| rejected())?;
    if intent.id != request.intent_id
        || intent.tenant_id != request.tenant_id
        || intent.destination_id != request.destination_id
        || intent.destination_version != request.destination_version
    {
        return Err(rejected());
    }
    Ok(intent)
}

pub async fn execute(
    db: &DatabaseConnection,
    ops: &DatabaseConnection,
    envelope: &SignedDeliveryRequest,
) -> KabiPayResult<SignedDeliveryResponse> {
    let request = &envelope.request;
    let binding = configured_binding(request.tenant_id)?;
    let secret = key(&binding.key_b64)?;
    verify_request(
        envelope,
        binding.tenant_id,
        binding.source_generation,
        &secret,
        Utc::now().timestamp(),
    )?;
    let tx = db.begin().await?;
    for setting in [
        "SET LOCAL lock_timeout='3s'",
        "SET LOCAL statement_timeout='5s'",
    ] {
        tx.execute_unprepared(setting).await?;
    }
    let authority = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1 AND source_database=current_database() FOR SHARE",
        [request.tenant_id.into()])).await?.ok_or_else(rejected)?;
    if authority.try_get::<i64>("", "producer_generation")? != request.source_generation {
        return Err(rejected());
    }
    let receipt = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT source_kind,resource_id,intent_hash,destination_version,source_generation,status,lease_generation,COALESCE(lease_until<=clock_timestamp(),true) AS lease_expired,claim_owner,outcome_code,acknowledgement_id FROM private_delivery_receipt WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 FOR UPDATE",
        [request.tenant_id.into(), request.intent_id.into(), request.destination_id.into()])).await?.ok_or_else(rejected)?;
    if receipt.try_get::<String>("", "intent_hash")? != request.intent_hash
        || receipt.try_get::<i64>("", "destination_version")? != request.destination_version
        || receipt.try_get::<i64>("", "source_generation")? != request.source_generation
    {
        return Err(rejected());
    }
    let mut status: String = receipt.try_get("", "status")?;
    let generation: i64 = receipt.try_get("", "lease_generation")?;
    let lease_expired: bool = receipt.try_get("", "lease_expired")?;
    let owner: Option<Uuid> = receipt.try_get("", "claim_owner")?;
    let mut code: String = receipt
        .try_get::<Option<String>>("", "outcome_code")?
        .unwrap_or_else(|| "pending".into());
    let ack: Option<Uuid> = receipt.try_get("", "acknowledgement_id")?;
    if status == "PROCESSING" && lease_expired {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE private_delivery_receipt SET status='UNCERTAIN',lease_until=NULL,claim_owner=NULL,outcome_code='prior_attempt_expired',updated_at=NOW() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND lease_generation=$4",
            [request.tenant_id.into(), request.intent_id.into(), request.destination_id.into(), generation.into()])).await?;
        status = "UNCERTAIN".into();
        code = "prior_attempt_expired".into();
    }
    match super::private_delivery_policy::lease_decision(
        &status,
        request.lease_generation,
        generation,
        owner == Some(request.request_id),
    ) {
        super::private_delivery_policy::LeaseDecision::ReadOnly => {
            tx.commit().await?;
            return signed_response(request, generation, &status, &code, ack, &secret);
        }
        super::private_delivery_policy::LeaseDecision::Reject => return Err(rejected()),
        super::private_delivery_policy::LeaseDecision::Claim => {}
    }
    let intent = load_intent(&tx, request).await?;
    if receipt.try_get::<String>("", "source_kind")? != intent.source_kind
        || receipt.try_get::<Uuid>("", "resource_id")? != intent.resource_id
    {
        return Err(rejected());
    }
    if intent.source_kind == "webhook" {
        let module = match intent.event_type.as_str() {
            "leave_request.approved" => "LEAVE",
            "expense.approved" => "EXPENSE",
            "employee_profile_change_request.submitted"
            | "employee_profile_change_request.cancelled"
            | "employee_profile_change_request.approved"
            | "employee_profile_change_request.rejected"
            | "employee_education.approved"
            | "employee_education.rejected"
            | "employee_work_experience.approved"
            | "employee_work_experience.rejected"
            | "employee.offboarded" => "EMPLOYEE",
            _ => return Err(rejected()),
        };
        let entitlements =
            kabipay_common::entitlements::Entitlements::load(ops, request.tenant_id).await?;
        if !entitlements.allows(module) {
            tx.commit().await?;
            return signed_response(
                request,
                generation,
                "PENDING",
                "module_disabled",
                None,
                &secret,
            );
        }
    }
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE private_delivery_receipt SET status='PROCESSING',lease_generation=$4,lease_until=clock_timestamp()+INTERVAL '45 seconds',claim_owner=$5,outcome_code='in_flight',updated_at=NOW() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3",
        [request.tenant_id.into(), request.intent_id.into(), request.destination_id.into(), request.lease_generation.into(), request.request_id.into()])).await?;
    tx.commit().await?;

    // This independent adapter transaction fences configuration/generation changes
    // across bounded HTTP; the originating business transaction already committed.
    let dispatch = db.begin().await?;
    for setting in [
        "SET LOCAL lock_timeout='3s'",
        "SET LOCAL statement_timeout='5s'",
        "SET LOCAL idle_in_transaction_session_timeout='30s'",
    ] {
        dispatch.execute_unprepared(setting).await?;
    }
    let authority = dispatch.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT producer_generation FROM private_source_cutover WHERE tenant_id=$1 AND source_database=current_database() FOR SHARE",
        [request.tenant_id.into()])).await?.ok_or_else(rejected)?;
    if authority.try_get::<i64>("", "producer_generation")? != request.source_generation {
        return Err(rejected());
    }
    let live = dispatch.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT intent_id FROM private_delivery_receipt WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND lease_generation=$4 AND claim_owner=$5 AND status='PROCESSING' AND lease_until>clock_timestamp() FOR UPDATE",
        [request.tenant_id.into(), request.intent_id.into(), request.destination_id.into(), request.lease_generation.into(), request.request_id.into()])).await?;
    if live.is_none() {
        return Err(unavailable());
    }
    let outcome = match intent.source_kind.as_str() {
        "finance_confirmation" | "finance_cancellation" => {
            execute_finance(&dispatch, &intent).await
        }
        "webhook" => execute_webhook(&dispatch, &intent, &binding).await,
        _ => Outcome::new("PERMANENT", "unsupported_source_kind"),
    };
    let acknowledgement = if outcome.status == "DELIVERED" {
        Some(Uuid::new_v4())
    } else {
        None
    };
    let result = dispatch.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE private_delivery_receipt r SET status=$6,outcome_code=$7,outcome_sha256=$8,acknowledgement_id=$9,accepted_at=CASE WHEN $6='DELIVERED' THEN clock_timestamp() ELSE NULL END,lease_until=NULL,claim_owner=NULL,updated_at=clock_timestamp() WHERE r.tenant_id=$1 AND r.intent_id=$2 AND r.destination_id=$3 AND r.lease_generation=$4 AND r.claim_owner=$5 AND r.status='PROCESSING' AND r.lease_until>clock_timestamp() AND r.source_generation=$10 AND EXISTS(SELECT 1 FROM private_source_cutover s WHERE s.tenant_id=r.tenant_id AND s.producer_generation=$10 AND s.source_database=current_database())",
        [request.tenant_id.into(), request.intent_id.into(), request.destination_id.into(), request.lease_generation.into(), request.request_id.into(), outcome.status.into(), outcome.code.into(), source_hash(outcome.code.as_bytes()).into(), acknowledgement.into(), request.source_generation.into()])).await?;
    if result.rows_affected() != 1 {
        return Err(unavailable());
    }
    dispatch.commit().await?;
    signed_response(
        request,
        request.lease_generation,
        outcome.status,
        outcome.code,
        acknowledgement,
        &secret,
    )
}

struct Outcome {
    status: &'static str,
    code: &'static str,
}
impl Outcome {
    fn new(status: &'static str, code: &'static str) -> Self {
        Self { status, code }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinanceAcknowledgement {
    operation_id: Uuid,
    state: FinanceAuthorizationState,
}

async fn execute_finance<C: ConnectionTrait>(db: &C, intent: &DeliveryIntentInput) -> Outcome {
    if intent.destination_id != PAYROLL_DESTINATION
        || intent.destination_version != 1
        || intent.payload != serde_json::json!({"operationId":intent.resource_id})
    {
        return Outcome::new("PERMANENT", "finance_identity_mismatch");
    }
    let (state, event, action, expected) = match intent.source_kind.as_str() {
        "finance_confirmation" => (
            HrOperationState::Committed,
            "EXIT_APPROVAL_COMMITTED",
            FinanceAction::ConfirmExitApproval,
            FinanceAuthorizationState::Confirmed,
        ),
        "finance_cancellation" => (
            HrOperationState::Cancelled,
            "EXIT_APPROVAL_CANCELLED",
            FinanceAction::CancelExitApproval,
            FinanceAuthorizationState::Cancelled,
        ),
        _ => return Outcome::new("PERMANENT", "finance_action_invalid"),
    };
    match super::finance_operation::find(db, intent.tenant_id, intent.resource_id, false).await {
        Ok(Some(operation)) if operation.state == state && intent.event_type == event => {}
        Ok(_) => return Outcome::new("PERMANENT", "finance_authority_mismatch"),
        Err(_) => return Outcome::new("RETRYABLE", "finance_authority_unavailable"),
    }
    // Existing signed transport retains authoritative Employee readback at Payroll.
    if !std::env::var("KABIPAY_FINANCE_PAYROLL_URL")
        .ok()
        .and_then(|url| reqwest::Url::parse(&url).ok())
        .is_some_and(|url| url.scheme() == "https")
    {
        return Outcome::new("PERMANENT", "finance_https_required");
    }
    match finance_transport::send(
        ServiceCaller::Employee,
        ServiceCaller::Payroll,
        intent.tenant_id,
        None,
        action,
        serde_json::json!({"operationId":intent.resource_id}),
    )
    .await
    {
        Ok(value) => match serde_json::from_value::<FinanceAcknowledgement>(value) {
            Ok(ack) if ack.operation_id == intent.resource_id && ack.state == expected => {
                Outcome::new("DELIVERED", "finance_signed_receipt_verified")
            }
            _ => Outcome::new("UNCERTAIN", "finance_receipt_mismatch"),
        },
        Err(_) => Outcome::new("UNCERTAIN", "finance_confirmation_unavailable"),
    }
}

async fn execute_webhook<C: ConnectionTrait>(
    db: &C,
    intent: &DeliveryIntentInput,
    binding: &Binding,
) -> Outcome {
    let (endpoint, key_ref) = if intent.destination_id == PLATFORM_DESTINATION {
        let Ok(url) = std::env::var("OUTBOX_WEBHOOK_URL") else {
            return Outcome::new("DISABLED", "platform_disabled");
        };
        if url.trim().is_empty() {
            return Outcome::new("DISABLED", "platform_disabled");
        }
        let version = std::env::var("KABIPAY_PLATFORM_WEBHOOK_VERSION")
            .ok()
            .and_then(|v| v.parse::<i64>().ok());
        if version != Some(intent.destination_version) {
            return Outcome::new("PERMANENT", "destination_version_changed");
        }
        (url, std::env::var("KABIPAY_PLATFORM_WEBHOOK_KEY_REF").ok())
    } else {
        let row = match db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT endpoint_url,signing_key_ref,config_version,is_active FROM webhook_subscription WHERE tenant_id=$1 AND id=$2 FOR SHARE",
            [intent.tenant_id.into(), intent.destination_id.into()])).await {
            Ok(Some(row)) => row, Ok(None) => return Outcome::new("DISABLED", "subscription_absent"),
            Err(_) => return Outcome::new("RETRYABLE", "destination_unavailable"),
        };
        if row.try_get::<bool>("", "is_active").ok() != Some(true) {
            return Outcome::new("DISABLED", "subscription_disabled");
        }
        if row.try_get::<i64>("", "config_version").ok() != Some(intent.destination_version) {
            return Outcome::new("PERMANENT", "destination_version_changed");
        }
        let Ok(url) = row.try_get::<String>("", "endpoint_url") else {
            return Outcome::new("PERMANENT", "destination_invalid");
        };
        (
            url,
            row.try_get::<Option<String>>("", "signing_key_ref")
                .ok()
                .flatten(),
        )
    };
    let wire_body = if intent.destination_id == PLATFORM_DESTINATION {
        let hash = match kabipay_common::private_webhooks::platform_configuration_hash(
            &endpoint,
            key_ref.as_deref(),
            intent.destination_version,
        ) {
            Ok(hash) => hash,
            Err(_) => return Outcome::new("PERMANENT", "destination_invalid"),
        };
        if intent
            .payload
            .get("destination_config_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(hash.as_str())
        {
            return Outcome::new("PERMANENT", "destination_configuration_changed");
        }
        let Some(body) = intent.payload.get("body") else {
            return Outcome::new("PERMANENT", "webhook_body_invalid");
        };
        body
    } else {
        &intent.payload
    };
    let signing =
        match key_ref.and_then(|id| binding.webhook_keys.iter().find(|key| key.key_id == id)) {
            Some(entry) => match key(&entry.key_b64) {
                Ok(key) => key,
                Err(_) => return Outcome::new("PERMANENT", "signing_key_invalid"),
            },
            None => return Outcome::new("PERMANENT", "signing_key_unreconciled"),
        };
    let url = match reqwest::Url::parse(&endpoint) {
        Ok(url)
            if url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none() =>
        {
            url
        }
        _ => return Outcome::new("PERMANENT", "destination_policy_denied"),
    };
    let Some(host) = url.host_str() else {
        return Outcome::new("PERMANENT", "destination_policy_denied");
    };
    if !super::private_egress_policy::allowed_host(host, &binding.allowed_webhook_hosts) {
        return Outcome::new("PERMANENT", "destination_policy_denied");
    }
    let addresses: Vec<_> =
        match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host, 443)))
            .await
        {
            Ok(Ok(addresses)) => addresses.take(33).collect(),
            _ => return Outcome::new("RETRYABLE", "dns_unavailable"),
        };
    if addresses.is_empty()
        || addresses.len() > 32
        || addresses
            .iter()
            .any(|address| !super::private_egress_policy::public_destination(address.ip()))
    {
        return Outcome::new("PERMANENT", "destination_address_denied");
    }
    let body = match serde_json::to_vec(wire_body) {
        Ok(body) if body.len() <= 131072 => body,
        _ => return Outcome::new("PERMANENT", "webhook_body_invalid"),
    };
    let timestamp = Utc::now().timestamp();
    let mut signature = match <Hmac<Sha256> as Mac>::new_from_slice(&signing) {
        Ok(mac) => mac,
        Err(_) => return Outcome::new("PERMANENT", "signing_key_invalid"),
    };
    signature.update(format!("{timestamp}.").as_bytes());
    signature.update(&body);
    let digest = signature.finalize().into_bytes();
    let signature = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .resolve_to_addrs(host, &addresses)
        .build()
    {
        Ok(client) => client,
        Err(_) => return Outcome::new("RETRYABLE", "egress_client_unavailable"),
    };
    match client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-KabiPay-Timestamp", timestamp.to_string())
        .header("X-KabiPay-Signature", format!("v1={signature}"))
        .header("X-KabiPay-Delivery-Id", intent.id.to_string())
        .header("Idempotency-Key", intent.id.to_string())
        .body(body)
        .send()
        .await
    {
        // Receiver body is deliberately never buffered or persisted; HTTP2xx retains v1 acceptance.
        Ok(response) => {
            let (status, code) =
                super::private_delivery_policy::http_outcome(response.status().as_u16());
            Outcome::new(status, code)
        }
        Err(_) => Outcome::new("UNCERTAIN", "webhook_acknowledgement_unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_node_rust_request_and_response_wire_vectors_match() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../integrations/organization-events/test/private-delivery-vector.json"
        )))
        .unwrap();
        let secret = STANDARD
            .decode(fixture["key_b64"].as_str().unwrap())
            .unwrap();
        let request: SignedDeliveryRequest =
            serde_json::from_value(fixture["request"].clone()).unwrap();
        assert_eq!(
            serde_json::to_string(&request.request).unwrap(),
            fixture["request_json"].as_str().unwrap()
        );
        verify_request(&request, request.request.tenant_id, 1, &secret, 100).unwrap();
        let response: SignedDeliveryResponse =
            serde_json::from_value(fixture["response"].clone()).unwrap();
        assert_eq!(
            serde_json::to_string(&response.response).unwrap(),
            fixture["response_json"].as_str().unwrap()
        );
        mac(
            b"kabipay-private-delivery-response-v1\0",
            &response.response,
            &secret,
        )
        .unwrap()
        .verify_slice(&URL_SAFE_NO_PAD.decode(response.signature).unwrap())
        .unwrap();
    }
    fn signed() -> SignedDeliveryRequest {
        let request = DeliveryRequest {
            version: 1,
            tenant_id: Uuid::from_u128(1),
            intent_id: Uuid::from_u128(2),
            destination_id: Uuid::from_u128(3),
            source_generation: 1,
            destination_version: 1,
            intent_hash: "a".repeat(64),
            lease_generation: 1,
            request_id: Uuid::from_u128(4),
            issued_at: 100,
        };
        let signature = URL_SAFE_NO_PAD.encode(
            mac(b"kabipay-private-delivery-request-v1\0", &request, &[7; 32])
                .unwrap()
                .finalize()
                .into_bytes(),
        );
        SignedDeliveryRequest { request, signature }
    }
    #[test]
    fn signed_reference_rejects_wrong_scope_time_and_each_mutated_identity() {
        let signed = signed();
        let tenant = signed.request.tenant_id;
        // Independent Node crypto.createHmac vector, exact field order from the wire contract.
        assert_eq!(
            signed.signature,
            "NZr8hRcBj-CIYzrwdGIey3UhXGlUIujwL2r9urOWKIw"
        );
        assert!(verify_request(&signed, tenant, 1, &[7; 32], 100).is_ok());
        assert!(verify_request(&signed, Uuid::new_v4(), 1, &[7; 32], 100).is_err());
        assert!(verify_request(&signed, tenant, 2, &[7; 32], 100).is_err());
        assert!(verify_request(&signed, tenant, 1, &[8; 32], 100).is_err());
        assert!(verify_request(&signed, tenant, 1, &[7; 32], 161).is_err());
        assert!(verify_request(&signed, tenant, 1, &[7; 32], 94).is_err());
        for field in 0..6 {
            let mut changed = super::tests::signed();
            match field {
                0 => changed.request.intent_id = Uuid::new_v4(),
                1 => changed.request.destination_id = Uuid::new_v4(),
                2 => changed.request.destination_version += 1,
                3 => changed.request.intent_hash = "b".repeat(64),
                4 => changed.request.lease_generation += 1,
                _ => changed.request.request_id = Uuid::new_v4(),
            }
            assert!(verify_request(&changed, tenant, 1, &[7; 32], 100).is_err());
        }
    }
    #[test]
    fn source_key_configuration_has_no_default_or_hash_to_secret_conversion() {
        let tenant = Uuid::from_u128(1);
        let binding = serde_json::json!({"tenant_id":tenant,"source_generation":1,"key_b64":STANDARD.encode([7;32]),"allowed_webhook_hosts":[],"webhook_keys":[]});
        let valid = serde_json::json!({"schema_version":1,"bindings":[binding.clone()]});
        assert!(binding_from_bytes(&serde_json::to_vec(&valid).unwrap(), tenant).is_ok());
        assert!(binding_from_bytes(&serde_json::to_vec(&valid).unwrap(), Uuid::new_v4()).is_err());
        assert!(binding_from_bytes(b"{}", tenant).is_err());
        assert!(binding_from_bytes(&vec![b' '; 65537], tenant).is_err());
        let duplicate =
            serde_json::json!({"schema_version":1,"bindings":[binding.clone(),binding]});
        assert!(binding_from_bytes(&serde_json::to_vec(&duplicate).unwrap(), tenant).is_err());
        for bad in ["", "0affzz", "not-base64@"] {
            let mut invalid = valid.clone();
            invalid["bindings"][0]["key_b64"] = bad.into();
            assert!(binding_from_bytes(&serde_json::to_vec(&invalid).unwrap(), tenant).is_err());
        }
    }
}
