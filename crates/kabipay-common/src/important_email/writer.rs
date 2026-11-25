use super::EmailEvent;
use crate::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendIntentResult {
    Inserted,
    ExistingSame,
}

/// Append inside the owning business transaction. Never opens a connection,
/// loads SMTP configuration, sends mail or swallows a failed database write.
pub async fn append_email_intent(
    tx: &DatabaseTransaction,
    event: &EmailEvent,
) -> KabiPayResult<AppendIntentResult> {
    let event = event.canonical()?;
    let hash = event.canonical_hash()?;
    let recipient_key = event.recipient.key();
    let inserted = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO important_email_intent \
        (id,tenant_id,event_id,recipient_key,event_hash,event,producer,resource_type,resource_id,resource_version,category,kind,occurred_at,expires_at) \
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
        ON CONFLICT(tenant_id,event_id,recipient_key) DO NOTHING RETURNING id,event_hash",
        [Uuid::new_v4().into(), event.tenant_id.into(), event.event_id.into(), recipient_key.clone().into(),
            hash.clone().into(), serde_json::to_value(&event)?.into(), event.producer().into(),
            event.resource_type.as_str().into(), event.resource_id.into(), event.resource_version.clone().into(),
            event.category.as_str().into(), event.kind.as_str().into(), event.occurred_at.into(), event.expires_at.into()]
    )).await?;
    if let Some(row) = inserted {
        let stored: String = row.try_get("", "event_hash")?;
        if stored == hash {
            return Ok(AppendIntentResult::Inserted);
        }
        return Err(conflict());
    }
    let existing = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT event_hash FROM important_email_intent WHERE tenant_id=$1 AND event_id=$2 AND recipient_key=$3",
        [event.tenant_id.into(), event.event_id.into(), recipient_key.into()]
    )).await?.ok_or_else(conflict)?;
    let stored: String = existing.try_get("", "event_hash")?;
    if stored != hash {
        return Err(conflict());
    }
    Ok(AppendIntentResult::ExistingSame)
}

fn conflict() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "EMAIL_IMMUTABLE_EVENT_CONFLICT",
        message: "important email event identity conflicts with its persisted transition".into(),
    }
}
