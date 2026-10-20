use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Lossless replacement for the legacy audit record fields, including unknown actions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditRecordInput {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub action: String,
    pub before_state: Option<Value>,
    pub after_state: Option<Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One immutable executable intent for one source owned destination.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryIntentInput {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub destination_id: Uuid,
    pub destination_version: i64,
    pub source_kind: String,
    pub resource_id: Uuid,
    pub event_type: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditHistoryFilter {
    pub tenant_id: Uuid,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    pub actions: Vec<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditHistoryPage {
    pub records: Vec<AuditRecordInput>,
    pub total: u64,
}
