//! Transactional per-destination webhook fanout. Caller owns the business transaction.
use crate::{
    private_events::{self, DeliveryIntentInput},
    KabiPayError, KabiPayResult,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const PLATFORM_DESTINATION: Uuid = Uuid::from_u128(0x00000000000040008000000000000001);
pub const NO_DESTINATION: Uuid = Uuid::from_u128(0x00000000000040008000000000000003);

pub struct LegacyWebhookEvent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub event_type: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}
const EVENTS: &[&str] = &[
    "leave_request.approved",
    "expense.approved",
    "employee_profile_change_request.submitted",
    "employee_profile_change_request.cancelled",
    "employee_profile_change_request.approved",
    "employee_profile_change_request.rejected",
    "employee_education.approved",
    "employee_education.rejected",
    "employee_work_experience.approved",
    "employee_work_experience.rejected",
    "employee.offboarded",
];

/// Stable event/destination identity, independent of fanout order and retry time.
pub fn intent_id(event: Uuid, destination: Uuid) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"kabipay-webhook-intent-v1\0");
    hash.update(event.as_bytes());
    hash.update(destination.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
pub fn platform_configuration_hash(
    endpoint: &str,
    key_ref: Option<&str>,
    version: i64,
) -> KabiPayResult<String> {
    Ok(private_events::crypto::source_hash(&serde_json::to_vec(
        &(endpoint, key_ref, version),
    )?))
}
fn matches_event(filter: &str, event: &str) -> bool {
    filter.trim() == "*" || filter.trim().eq_ignore_ascii_case(event)
}
fn webhook_body(event: &LegacyWebhookEvent) -> KabiPayResult<(Value, DateTime<Utc>)> {
    let occurred_at = private_events::canonical_private_time(event.created_at)?;
    Ok((
        json!({"schema_version":1,"outbox_id":event.id,"tenant_id":event.tenant_id,
        "aggregate_type":event.aggregate_type,"aggregate_id":event.aggregate_id,
        "event_type":event.event_type,"payload":event.payload,"created_at":occurred_at}),
        occurred_at,
    ))
}

pub async fn enqueue_webhooks<C: ConnectionTrait>(
    db: &C,
    event: LegacyWebhookEvent,
) -> KabiPayResult<Uuid> {
    if event.id.is_nil()
        || event.tenant_id.is_nil()
        || event.aggregate_id.is_nil()
        || !EVENTS.contains(&event.event_type.as_str())
    {
        return Err(KabiPayError::Validation(
            "Unsupported webhook event identity".into(),
        ));
    }
    let (body, occurred_at) = webhook_body(&event)?;
    let skip_subscriptions = matches!(
        std::env::var("OUTBOX_SKIP_TENANT_WEBHOOKS").as_deref(),
        Ok("1" | "true" | "yes")
    );
    let mut destinations = Vec::new();
    if !skip_subscriptions {
        // Share locks hold the destination version until the originating transaction commits.
        let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT id,event_name,config_version FROM webhook_subscription WHERE tenant_id=$1 AND is_active=true ORDER BY created_at,id LIMIT 257 FOR SHARE",
            [event.tenant_id.into()])).await?;
        if rows.len() > 256 {
            return Err(KabiPayError::Validation(
                "Webhook fanout exceeds the source destination bound".into(),
            ));
        }
        for row in rows {
            if matches_event(&row.try_get::<String>("", "event_name")?, &event.event_type) {
                destinations.push((
                    row.try_get::<Uuid>("", "id")?,
                    row.try_get::<i64>("", "config_version")?,
                ));
            }
        }
    }
    if std::env::var("OUTBOX_WEBHOOK_URL")
        .ok()
        .is_some_and(|url| !url.trim().is_empty())
    {
        let version = std::env::var("KABIPAY_PLATFORM_WEBHOOK_VERSION")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .ok_or_else(|| {
                KabiPayError::Validation("Platform webhook version must be configured".into())
            })?;
        destinations.push((PLATFORM_DESTINATION, version));
    }
    let no_destination = destinations.is_empty();
    if no_destination {
        destinations.push((NO_DESTINATION, 1));
    }
    for (destination, version) in destinations {
        let payload = if destination == PLATFORM_DESTINATION {
            let endpoint = std::env::var("OUTBOX_WEBHOOK_URL").map_err(|_| {
                KabiPayError::Validation("Platform webhook configuration changed".into())
            })?;
            let key_ref = std::env::var("KABIPAY_PLATFORM_WEBHOOK_KEY_REF").ok();
            json!({"body":body,"destination_config_sha256":platform_configuration_hash(&endpoint,key_ref.as_deref(),version)?})
        } else {
            body.clone()
        };
        let input = DeliveryIntentInput {
            id: intent_id(event.id, destination),
            tenant_id: event.tenant_id,
            destination_id: destination,
            destination_version: version,
            source_kind: "webhook".into(),
            resource_id: event.id,
            event_type: event.event_type.clone(),
            payload,
            occurred_at,
        };
        private_events::enqueue_delivery(db, &input).await?;
        if no_destination {
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE private_delivery_receipt SET status='NO_DESTINATION',outcome_code='no_configured_destination',updated_at=NOW() WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3 AND status='PENDING'",
                [event.tenant_id.into(), input.id.into(), destination.into()])).await?;
        }
    }
    Ok(event.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webhook_body_and_intent_time_use_the_same_canonical_microsecond() {
        let raw = DateTime::parse_from_rfc3339("2026-10-19T00:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let event = LegacyWebhookEvent {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            aggregate_type: "expense".into(),
            aggregate_id: Uuid::new_v4(),
            event_type: "expense.approved".into(),
            payload: json!({"original":true}),
            created_at: raw,
        };
        let (body, time) = webhook_body(&event).unwrap();
        let wire: DateTime<Utc> = serde_json::from_value(body["created_at"].clone()).unwrap();
        assert_eq!(wire, time);
        assert_eq!(time.timestamp_subsec_nanos(), 123456000);
        assert_eq!(body["outbox_id"], json!(event.id));
        assert_eq!(body["payload"], event.payload);
    }
    #[test]
    fn stable_fanout_identity_and_exact_legacy_event_names() {
        let event = Uuid::new_v4();
        let destination = Uuid::new_v4();
        assert_eq!(intent_id(event, destination), intent_id(event, destination));
        assert_ne!(
            intent_id(event, destination),
            intent_id(event, Uuid::new_v4())
        );
        assert_ne!(
            intent_id(event, destination),
            intent_id(Uuid::new_v4(), destination)
        );
        assert_eq!(EVENTS.len(), 11);
        assert!(EVENTS.contains(&"employee.offboarded"));
        assert!(matches_event("*", "employee.offboarded"));
        assert!(matches_event(
            " LEAVE_REQUEST.APPROVED ",
            "leave_request.approved"
        ));
        assert!(!matches_event("leave_request.approved", "expense.approved"));
    }
    #[test]
    fn platform_config_changes_cannot_redirect_an_existing_immutable_intent() {
        let original =
            platform_configuration_hash("https://hooks.example.com", Some("key-v1"), 1).unwrap();
        for changed in [
            platform_configuration_hash("https://other.example.com", Some("key-v1"), 1).unwrap(),
            platform_configuration_hash("https://hooks.example.com", Some("key-v2"), 1).unwrap(),
            platform_configuration_hash("https://hooks.example.com", Some("key-v1"), 2).unwrap(),
            platform_configuration_hash("https://hooks.example.com", None, 1).unwrap(),
        ] {
            assert_ne!(original, changed);
        }
    }
}
