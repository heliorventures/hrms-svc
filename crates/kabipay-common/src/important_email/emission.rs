//! Event construction uses the persisted transition time/revision, never a
//! fresh dispatch clock or an untrusted recipient address.
use super::*;
use crate::KabiPayResult;
use chrono::{DateTime, Duration, Utc};
use sea_orm::DatabaseTransaction;
use std::collections::HashSet;
use uuid::Uuid;

pub struct EmailTransition {
    pub tenant_id: Uuid,
    pub resource_type: EmailResourceType,
    pub resource_id: Uuid,
    pub resource_version: String,
    pub occurred_at: DateTime<Utc>,
    pub actor_ref: Option<EmailActorRef>,
    /// Ephemeral verified target-company aliases; never part of the stored event.
    pub actor_target_user_ids: Vec<Uuid>,
}
impl EmailTransition {
    pub fn event(
        &self,
        kind: EmailEventKind,
        reviewer: EmailReviewer,
    ) -> KabiPayResult<Option<EmailEvent>> {
        if matches!(
            kind,
            EmailEventKind::ActionRequired | EmailEventKind::Outcome
        ) {
            if matches!(&reviewer.recipient, EmailRecipientRef::LocalUser { user_id } if self.actor_target_user_ids.contains(user_id))
            {
                return Ok(None);
            }
            if self
                .actor_ref
                .as_ref()
                .is_some_and(|actor| same_email_actor(actor, &reviewer.recipient))
            {
                return Ok(None);
            }
        }
        let category = match self.resource_type {
            EmailResourceType::Leave => EmailCategory::Leave,
            EmailResourceType::Payslip => EmailCategory::Payslip,
            EmailResourceType::Attendance => EmailCategory::MissedPunchOut,
            EmailResourceType::Test => EmailCategory::Test,
            _ => EmailCategory::Approval,
        };
        let event = EmailEvent {
            event_id: stable_event_id(
                self.tenant_id,
                self.resource_type,
                self.resource_id,
                &self.resource_version,
                kind,
            ),
            tenant_id: self.tenant_id,
            resource_type: self.resource_type,
            resource_id: self.resource_id,
            resource_version: self.resource_version.clone(),
            category,
            kind,
            template_version: 1,
            recipient: reviewer.recipient,
            actor_ref: self.actor_ref.clone(),
            authorization_ref: reviewer.authorization_ref,
            action_target: reviewer.action_target,
            occurred_at: self.occurred_at,
            expires_at: self.occurred_at
                + if kind == EmailEventKind::ActionRequired {
                    Duration::hours(24)
                } else {
                    Duration::days(7)
                },
        };
        event.validate()?;
        Ok(Some(event))
    }
}
pub fn same_email_actor(actor: &EmailActorRef, recipient: &EmailRecipientRef) -> bool {
    match (actor, recipient) {
        (EmailActorRef::LocalUser { user_id: a }, EmailRecipientRef::LocalUser { user_id: b }) => {
            a == b
        }
        (
            EmailActorRef::OrganizationPrincipal { principal_id: a },
            EmailRecipientRef::OrganizationPrincipal {
                principal_id: b, ..
            },
        ) => a == b,
        _ => false,
    }
}
pub async fn append_transition_emails(
    tx: &DatabaseTransaction,
    transition: &EmailTransition,
    kind: EmailEventKind,
    reviewers: Vec<EmailReviewer>,
) -> KabiPayResult<usize> {
    let mut recipients = HashSet::new();
    let mut appended = 0;
    for reviewer in reviewers {
        if !recipients.insert(reviewer.recipient.key()) {
            continue;
        }
        if let Some(event) = transition.event(kind, reviewer)? {
            append_email_intent(tx, &event).await?;
            appended += 1;
        }
    }
    Ok(appended)
}

/// Direct (non-workflow) review stages use the same bounded permission/scope
/// selector as workflow producers. No mailbox or secret is read here.
pub async fn append_local_review_emails(
    tx: &DatabaseTransaction,
    transition: &EmailTransition,
    subject: Uuid,
    permissions: &[&str],
    allowed_scopes: &[crate::context::ScopeType],
    transition_key: &str,
) -> KabiPayResult<usize> {
    let mut reviewers = Vec::new();
    for permission in permissions {
        for actor in
            select_local_email_reviewers(tx, transition.tenant_id, subject, permission).await?
        {
            if allowed_scopes.contains(&actor.scope) {
                reviewers.push(local_transition_reviewer(
                    transition,
                    actor.user_id,
                    transition_key,
                ));
            }
        }
    }
    append_transition_emails(tx, transition, EmailEventKind::ActionRequired, reviewers).await
}
pub fn local_transition_reviewer(
    transition: &EmailTransition,
    user_id: Uuid,
    key: &str,
) -> EmailReviewer {
    EmailReviewer {
        recipient: EmailRecipientRef::LocalUser { user_id },
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: transition.resource_type,
            transition_key: key.into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: transition.resource_type,
            resource_id: transition.resource_id,
        },
    }
}

/// Separate from employment-bound workflow approval: the existing directory
/// and program administration APIs permit ALL-scope accounts without employment.
pub async fn append_account_review_emails(
    tx: &DatabaseTransaction,
    transition: &EmailTransition,
    subject: Uuid,
    permissions: &[&str],
    key: &str,
) -> KabiPayResult<usize> {
    let mut reviewers = Vec::new();
    for permission in permissions {
        for user in
            select_all_scope_email_users(tx, transition.tenant_id, subject, permission).await?
        {
            reviewers.push(local_transition_reviewer(transition, user, key));
        }
    }
    append_transition_emails(tx, transition, EmailEventKind::ActionRequired, reviewers).await
}
