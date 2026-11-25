//! Tenant-local durable delivery. Every active mutation is fenced by a token;
//! SQL never stores an address, rendered message or provider response.
use super::smtp::{SmtpErrorCode, SmtpOutcome};
use chrono::{DateTime, Duration, Utc};
use kabipay_common::important_email::EmailCategory;
use kabipay_common::{
    important_email::{EmailEvent, EmailRecipientIdentity},
    KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

/// attempt_count counts completed known-safe retries, not initial acquisition.
/// Five retries permit the initial attempt plus at most five subsequent attempts.
pub fn retry_at(
    retries: i32,
    created: DateTime<Utc>,
    expiry: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !(0..5).contains(&retries) {
        return None;
    }
    let next = now + Duration::seconds((30_i64 * (1_i64 << retries)).min(1800));
    (next < created + Duration::hours(24) && next < expiry).then_some(next)
}
pub struct DeliveryCutoffs {
    enabled_after: Option<DateTime<Utc>>,
    categories: Vec<(EmailCategory, DateTime<Utc>)>,
}
impl DeliveryCutoffs {
    pub fn disabled() -> Self {
        Self {
            enabled_after: None,
            categories: Vec::new(),
        }
    }
    pub fn enabled(
        after: DateTime<Utc>,
        categories: impl IntoIterator<Item = (EmailCategory, DateTime<Utc>)>,
    ) -> Self {
        Self {
            enabled_after: Some(after),
            categories: categories.into_iter().collect(),
        }
    }
    pub fn admits(
        &self,
        category: EmailCategory,
        occurred: DateTime<Utc>,
        created: DateTime<Utc>,
    ) -> bool {
        self.enabled_after.is_some_and(|enabled| {
            self.categories
                .iter()
                .find(|(kind, _)| *kind == category)
                .is_some_and(|(_, cutoff)| {
                    occurred >= enabled.max(*cutoff) && created >= enabled.max(*cutoff)
                })
        })
    }
}

#[derive(Clone)]
pub struct DeliveryAttempt {
    pub id: Uuid,
    pub tenant: Uuid,
    pub event: EmailEvent,
    pub event_hash: String,
    pub created_at: DateTime<Utc>,
    pub settings_version: i64,
    token: Uuid,
    retries: i32,
}
impl std::fmt::Debug for DeliveryAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryAttempt")
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}
pub struct ConfirmedSubmissionFence {
    _private: (),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityReservation {
    Ready,
    Duplicate,
    Stale,
}
#[derive(Clone, Copy, Debug)]
pub enum DeliveryCode {
    Disabled,
    Expired,
    RecipientUnavailable,
    Obsolete,
    Forbidden,
    SettingsChanged,
    Duplicate,
    RoutingUnavailable,
    KeyUnavailable,
    CredentialUnavailable,
    DestinationRefused,
}
impl DeliveryCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "EMAIL_DISABLED",
            Self::Expired => "EMAIL_EVENT_EXPIRED",
            Self::RecipientUnavailable => "EMAIL_RECIPIENT_UNAVAILABLE",
            Self::Obsolete => "EMAIL_EVENT_OBSOLETE",
            Self::Forbidden => "EMAIL_TEST_AUTHORITY_REVOKED",
            Self::SettingsChanged => "EMAIL_SETTINGS_CHANGED",
            Self::Duplicate => "EMAIL_RESOLVED_DUPLICATE",
            Self::RoutingUnavailable => "EMAIL_ROUTING_UNAVAILABLE",
            Self::KeyUnavailable => "EMAIL_KEY_UNAVAILABLE",
            Self::CredentialUnavailable => "EMAIL_CREDENTIAL_UNAVAILABLE",
            Self::DestinationRefused => "EMAIL_DESTINATION_REFUSED",
        }
    }
}
fn sql(query: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}
fn invalid() -> KabiPayError {
    KabiPayError::Internal("EMAIL_DELIVERY_STATE_INVALID".into())
}

/// DB time owns leases and retention. No caller clock can renew expired work.
pub async fn claim_email_batch(
    db: &DatabaseConnection,
    tenant: Uuid,
    limit: usize,
) -> KabiPayResult<Vec<DeliveryAttempt>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let tx = db.begin().await?;
    let settings = tx
        .query_one(sql(
            "SELECT version FROM tenant_email_settings WHERE tenant_id=$1 FOR UPDATE",
            vec![tenant.into()],
        ))
        .await?
        .ok_or_else(invalid)?;
    let version: i64 = settings.try_get("", "version")?;
    // At most two live leases exist. A crash after the body fence is never reset.
    tx.execute(sql("UPDATE important_email_intent SET state='UNCERTAIN',terminal_at=clock_timestamp(),lease_token=NULL,lease_until=NULL,last_error_code='EMAIL_SUBMISSION_UNCERTAIN' WHERE tenant_id=$1 AND state='SUBMITTING' AND lease_until<=clock_timestamp()",vec![tenant.into()])).await?;
    tx.execute(sql("UPDATE important_email_intent SET state=CASE WHEN attempt_count<5 AND clock_timestamp()+make_interval(secs=>30*power(2,attempt_count)::int)<LEAST(created_at+interval '24 hours',expires_at) THEN 'PENDING' ELSE 'FAILED' END,attempt_count=attempt_count+CASE WHEN attempt_count<5 AND clock_timestamp()+make_interval(secs=>30*power(2,attempt_count)::int)<LEAST(created_at+interval '24 hours',expires_at) THEN 1 ELSE 0 END,next_attempt_at=clock_timestamp()+make_interval(secs=>30*power(2,attempt_count)::int),terminal_at=CASE WHEN attempt_count<5 AND clock_timestamp()+make_interval(secs=>30*power(2,attempt_count)::int)<LEAST(created_at+interval '24 hours',expires_at) THEN NULL ELSE clock_timestamp() END,lease_token=NULL,lease_until=NULL,last_error_code='EMAIL_SMTP_UNAVAILABLE' WHERE tenant_id=$1 AND state='CLAIMED' AND lease_until<=clock_timestamp()",vec![tenant.into()])).await?;
    let active:i64=tx.query_one(sql("SELECT count(*) AS n FROM important_email_intent WHERE tenant_id=$1 AND state IN('CLAIMED','SUBMITTING') AND lease_until>clock_timestamp()",vec![tenant.into()])).await?.ok_or_else(invalid)?.try_get("","n")?;
    let take = (2 - active).max(0).min(limit.min(20) as i64);
    let rows=tx.query_all(sql("SELECT id,event,event_hash,created_at,attempt_count,expires_at FROM important_email_intent WHERE tenant_id=$1 AND state='PENDING' AND next_attempt_at<=clock_timestamp() ORDER BY next_attempt_at,created_at,id LIMIT $2 FOR UPDATE SKIP LOCKED",vec![tenant.into(),take.into()])).await?;
    let mut attempts = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        let token = Uuid::new_v4();
        let event: EmailEvent =
            serde_json::from_value(row.try_get("", "event")?).map_err(|_| invalid())?;
        let hash: String = row.try_get("", "event_hash")?;
        if event.tenant_id != tenant || event.canonical_hash()? != hash {
            return Err(invalid());
        }
        tx.execute(sql("UPDATE important_email_intent SET state='CLAIMED',lease_token=$3,lease_until=clock_timestamp()+interval '90 seconds',settings_version=$4 WHERE tenant_id=$1 AND id=$2 AND state='PENDING'",vec![tenant.into(),id.into(),token.into(),version.into()])).await?;
        attempts.push(DeliveryAttempt {
            id,
            tenant,
            event,
            event_hash: hash,
            created_at: row.try_get("", "created_at")?,
            settings_version: version,
            token,
            retries: row.try_get("", "attempt_count")?,
        });
    }
    tx.commit().await?;
    Ok(attempts)
}
pub async fn heartbeat(db: &DatabaseConnection, attempt: &DeliveryAttempt) -> KabiPayResult<bool> {
    Ok(db.execute(sql("UPDATE important_email_intent SET lease_until=clock_timestamp()+interval '90 seconds' WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND settings_version=$4 AND state='CLAIMED' AND lease_until>clock_timestamp() AND EXISTS(SELECT 1 FROM tenant_email_settings s WHERE s.tenant_id=$1 AND s.version=$4)",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),attempt.settings_version.into()])).await?.rows_affected()==1)
}
pub async fn reserve_identity(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    identity: &EmailRecipientIdentity,
) -> KabiPayResult<IdentityReservation> {
    if identity.source_tenant_id.is_nil() || identity.source_user_id.is_nil() {
        return Err(invalid());
    }
    let tx = db.begin().await?;
    // Different local/organization aliases serialize on the verified delivery key.
    tx.execute(sql(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,134))",
        vec![format!(
            "{}:{}:{}:{}",
            attempt.tenant,
            attempt.event.event_id,
            identity.source_tenant_id,
            identity.source_user_id
        )
        .into()],
    ))
    .await?;
    if tx.query_one(sql("SELECT id FROM important_email_intent WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND settings_version=$4 AND state='CLAIMED' AND lease_until>clock_timestamp() FOR UPDATE",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),attempt.settings_version.into()])).await?.is_none(){return Ok(IdentityReservation::Stale);}
    let duplicate=tx.query_one(sql("SELECT id FROM important_email_intent WHERE tenant_id=$1 AND event_id=$2 AND delivery_source_tenant=$3 AND delivery_source_user=$4 AND id<>$5 LIMIT 1",vec![attempt.tenant.into(),attempt.event.event_id.into(),identity.source_tenant_id.into(),identity.source_user_id.into(),attempt.id.into()])).await?.is_some();
    if duplicate {
        tx.execute(sql("UPDATE important_email_intent SET state='SKIPPED',terminal_at=clock_timestamp(),lease_token=NULL,lease_until=NULL,last_error_code='EMAIL_RESOLVED_DUPLICATE' WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND state='CLAIMED'",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into()])).await?;
    } else {
        tx.execute(sql("UPDATE important_email_intent SET delivery_source_tenant=$4,delivery_source_user=$5 WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND state='CLAIMED'",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),identity.source_tenant_id.into(),identity.source_user_id.into()])).await?;
    }
    tx.commit().await?;
    Ok(if duplicate {
        IdentityReservation::Duplicate
    } else {
        IdentityReservation::Ready
    })
}
pub async fn mark_submitting(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
) -> KabiPayResult<Option<ConfirmedSubmissionFence>> {
    let tx = db.begin().await?;
    // Match claim/settings-save lock order: settings before intent.
    tx.query_one(sql(
        "SELECT version FROM tenant_email_settings WHERE tenant_id=$1 FOR UPDATE",
        vec![attempt.tenant.into()],
    ))
    .await?
    .ok_or_else(invalid)?;
    let changed=tx.execute(sql("UPDATE important_email_intent SET state='SUBMITTING' WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND settings_version=$4 AND state='CLAIMED' AND lease_until>clock_timestamp() AND EXISTS(SELECT 1 FROM tenant_email_settings s WHERE s.tenant_id=$1 AND s.version=$4)",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),attempt.settings_version.into()])).await?.rows_affected();
    tx.commit().await?;
    Ok((changed == 1).then_some(ConfirmedSubmissionFence { _private: () }))
}
async fn terminal(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    state: &str,
    code: Option<&str>,
    expected: &str,
) -> KabiPayResult<bool> {
    Ok(db.execute(sql("UPDATE important_email_intent SET state=$5,terminal_at=clock_timestamp(),lease_token=NULL,lease_until=NULL,last_error_code=$6 WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND settings_version=$4 AND state=ANY(string_to_array($7,','))",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),attempt.settings_version.into(),state.into(),code.into(),expected.into()])).await?.rows_affected()==1)
}
pub async fn skip_email_attempt(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    code: DeliveryCode,
) -> KabiPayResult<bool> {
    terminal(db, attempt, "SKIPPED", Some(code.as_str()), "CLAIMED").await
}
pub async fn retry_email_attempt(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    code: &'static str,
    after_submission: bool,
) -> KabiPayResult<bool> {
    // The only after-fence retry is an explicit known-negative SMTP reply.
    if after_submission && code != SmtpErrorCode::Unavailable.as_str() {
        return Err(invalid());
    }
    let now: DateTime<Utc> = db
        .query_one(sql("SELECT clock_timestamp() AS moment", vec![]))
        .await?
        .ok_or_else(invalid)?
        .try_get("", "moment")?;
    let Some(next) = retry_at(
        attempt.retries,
        attempt.created_at,
        attempt.event.expires_at,
        now,
    ) else {
        return terminal(
            db,
            attempt,
            "FAILED",
            Some(code),
            if after_submission {
                "CLAIMED,SUBMITTING"
            } else {
                "CLAIMED"
            },
        )
        .await;
    };
    Ok(db.execute(sql("UPDATE important_email_intent SET state='PENDING',attempt_count=attempt_count+1,next_attempt_at=$5,lease_token=NULL,lease_until=NULL,last_error_code=$6 WHERE tenant_id=$1 AND id=$2 AND lease_token=$3 AND settings_version=$4 AND state=ANY(string_to_array($7,',')) AND lease_until>clock_timestamp()",vec![attempt.tenant.into(),attempt.id.into(),attempt.token.into(),attempt.settings_version.into(),next.into(),code.into(),if after_submission{"CLAIMED,SUBMITTING"}else{"CLAIMED"}.into()])).await?.rows_affected()==1)
}
pub async fn complete_email_attempt(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    outcome: SmtpOutcome,
) -> KabiPayResult<bool> {
    match outcome {
        SmtpOutcome::Accepted => terminal(db, attempt, "ACCEPTED", None, "SUBMITTING").await,
        SmtpOutcome::Uncertain(code) => {
            terminal(db, attempt, "UNCERTAIN", Some(code.as_str()), "SUBMITTING").await
        }
        SmtpOutcome::NotSubmitted(code) => {
            terminal(db, attempt, "SKIPPED", Some(code.as_str()), "CLAIMED").await
        }
        SmtpOutcome::Rejected(code) => {
            terminal(
                db,
                attempt,
                "FAILED",
                Some(code.as_str()),
                "CLAIMED,SUBMITTING",
            )
            .await
        }
        SmtpOutcome::RetrySafe(code) => retry_email_attempt(db, attempt, code.as_str(), true).await,
    }
}
pub const PURGE_TERMINAL_SQL: &str = r#"DELETE FROM important_email_intent WHERE tenant_id=$1 AND id IN(SELECT id FROM important_email_intent WHERE tenant_id=$1 AND terminal_at<clock_timestamp()-interval '30 days' ORDER BY terminal_at,id LIMIT 100)"#;
pub async fn purge_terminal(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<u64> {
    Ok(db
        .execute(sql(PURGE_TERMINAL_SQL, vec![tenant.into()]))
        .await?
        .rows_affected())
}
