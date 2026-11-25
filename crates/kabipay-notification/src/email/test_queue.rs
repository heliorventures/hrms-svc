//! Fixed, actor-bound tests. A queued request never accepts a mailbox or message.
use super::{
    settings::{configuration_unavailable, public_row, version_conflict, PUBLIC_PROJECTION},
    settings_types::valid_mailbox,
};
use async_graphql::{Enum, SimpleObject};
use chrono::{DateTime, Duration, Utc};
use kabipay_common::{
    important_email::{
        append_email_intent, EmailActionTarget, EmailAuthorizationRef, EmailCategory, EmailEvent,
        EmailEventKind, EmailRecipientRef, EmailResourceType,
    },
    KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub enum EmailTestState {
    Queued,
    Accepted,
    Failed,
    Uncertain,
    Skipped,
}
#[derive(Clone, Debug, SimpleObject)]
pub struct EmailTestStatus {
    pub id: Uuid,
    pub state: EmailTestState,
    pub code: Option<String>,
}

pub async fn queue_test(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    expected_version: i64,
) -> KabiPayResult<EmailTestStatus> {
    let tx = db.begin().await?;
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,132))",
        [format!("tenant-email-test:{tenant}").into()],
    ))
    .await?;
    let row = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT {PUBLIC_PROJECTION} FROM tenant_email_settings WHERE tenant_id=$1 FOR UPDATE"),
        [tenant.into()])).await?.ok_or_else(configuration_unavailable)?;
    let settings = public_row(&row)?;
    if expected_version < 1 || expected_version != settings.version {
        return Err(version_conflict());
    }
    if !settings.credential_present || !valid_mailbox(&settings.account_email) {
        return Err(configuration_unavailable());
    }
    let recipient = EmailRecipientRef::LocalUser { user_id: actor };
    let counts = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT count(*) AS tenant_count,count(*) FILTER(WHERE recipient_key=$2 AND occurred_at>clock_timestamp()-interval '15 minutes') AS actor_count,clock_timestamp() AS email_now FROM important_email_intent WHERE tenant_id=$1 AND category='test' AND occurred_at>clock_timestamp()-interval '1 hour'",
        [tenant.into(), recipient.key().into()])).await?.ok_or_else(configuration_unavailable)?;
    let tenant_count: i64 = counts.try_get("", "tenant_count")?;
    let actor_count: i64 = counts.try_get("", "actor_count")?;
    if tenant_count >= 10 || actor_count >= 3 {
        return Err(KabiPayError::BusinessRule {
            code: "EMAIL_TEST_RATE_LIMITED",
            message: "Wait before sending another email test.".into(),
        });
    }
    let now: DateTime<Utc> = counts.try_get("", "email_now")?;
    let event_id = Uuid::new_v4();
    append_email_intent(
        &tx,
        &EmailEvent {
            event_id,
            tenant_id: tenant,
            resource_type: EmailResourceType::Test,
            resource_id: settings.id,
            resource_version: settings.version.to_string(),
            category: EmailCategory::Test,
            kind: EmailEventKind::Test,
            template_version: 1,
            recipient: recipient.clone(),
            actor_ref: Some(recipient.into()),
            authorization_ref: EmailAuthorizationRef::DomainTransition {
                resource_type: EmailResourceType::Test,
                transition_key: "saved-smtp-account-test".into(),
            },
            action_target: EmailActionTarget::CompanyResource {
                resource_type: EmailResourceType::Test,
                resource_id: settings.id,
            },
            occurred_at: now,
            expires_at: now + Duration::hours(24),
        },
    )
    .await?;
    let row = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id FROM important_email_intent WHERE tenant_id=$1 AND event_id=$2 AND recipient_key=$3",
        [tenant.into(), event_id.into(), EmailRecipientRef::LocalUser { user_id: actor }.key().into()]
    )).await?.ok_or_else(configuration_unavailable)?;
    let id = row.try_get("", "id")?;
    tx.commit().await?;
    Ok(EmailTestStatus {
        id,
        state: EmailTestState::Queued,
        code: None,
    })
}

pub async fn load_test_status(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    id: Uuid,
) -> KabiPayResult<EmailTestStatus> {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,state,last_error_code FROM important_email_intent WHERE tenant_id=$1 AND id=$2 AND category='test' AND recipient_key=$3",
        [tenant.into(), id.into(), EmailRecipientRef::LocalUser { user_id: actor }.key().into()]
    )).await?.ok_or_else(|| KabiPayError::NotFound { entity: "emailSettingsTest", id: id.to_string() })?;
    let state: String = row.try_get("", "state")?;
    let state = match state.as_str() {
        "PENDING" | "CLAIMED" | "SUBMITTING" => EmailTestState::Queued,
        "ACCEPTED" => EmailTestState::Accepted,
        "UNCERTAIN" => EmailTestState::Uncertain,
        "SKIPPED" => EmailTestState::Skipped,
        _ => EmailTestState::Failed,
    };
    // Never echo database/provider text; the worker stores fixed codes only, and
    // this public projection independently allowlists its channel's codes.
    let stored: Option<String> = row.try_get("", "last_error_code")?;
    let code = stored.filter(|code| {
        matches!(
            code.as_str(),
            "EMAIL_KEY_UNAVAILABLE"
                | "EMAIL_CREDENTIAL_UNAVAILABLE"
                | "EMAIL_SMTP_AUTHENTICATION"
                | "EMAIL_SMTP_UNAVAILABLE"
                | "EMAIL_SMTP_REJECTED"
                | "EMAIL_SUBMISSION_UNCERTAIN"
                | "EMAIL_SETTINGS_CHANGED"
                | "EMAIL_RECIPIENT_UNAVAILABLE"
                | "EMAIL_TEST_AUTHORITY_REVOKED"
                | "EMAIL_DESTINATION_REFUSED"
                | "EMAIL_EVENT_EXPIRED"
        )
    });
    Ok(EmailTestStatus { id, state, code })
}
