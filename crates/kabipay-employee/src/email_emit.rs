//! Profile email metadata contains no decrypted requested/current value.
use chrono::{DateTime, Utc};
use kabipay_common::{important_email::*, KabiPayResult};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;
pub async fn emit_profile_email(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
    employee: Uuid,
    requester: Uuid,
    actor: Uuid,
    status: &str,
    at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    let transition = EmailTransition {
        tenant_id: tenant,
        resource_type: EmailResourceType::ProfileChange,
        resource_id: id,
        resource_version: email_timestamp_version(status, at),
        occurred_at: at,
        actor_ref: Some(EmailActorRef::LocalUser { user_id: actor }),
        actor_target_user_ids: vec![],
    };
    if status == "PENDING" {
        append_account_review_emails(
            tx,
            &transition,
            employee,
            &["employee:write", "employee:manage"],
            "pending",
        )
        .await
    } else if matches!(status, "APPROVED" | "REJECTED") {
        append_transition_emails(
            tx,
            &transition,
            EmailEventKind::Outcome,
            vec![local_transition_reviewer(
                &transition,
                requester,
                &status.to_ascii_lowercase(),
            )],
        )
        .await
    } else {
        Ok(0)
    }
}
