//! Transaction-owned actionable participant assignments/outcomes, never a stage
//! broadcast. Candidate selection reads only current identity/authority metadata.
use kabipay_common::{context::ScopeType, important_email::*, KabiPayResult};
use kabipay_organization_tenant::{
    email_reviewers::select_organization_email_reviewers, statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;
#[derive(Clone, Copy)]
pub enum PerformanceEmailChange {
    Action,
    GoalsOutcome,
    ReviewOutcome,
}
pub async fn emit_performance_cycle(
    tx: &DatabaseTransaction,
    metadata: &DatabaseConnection,
    tenant: Uuid,
    cycle: Uuid,
    actor: Option<EmailActorRef>,
) -> KabiPayResult<usize> {
    let rows=tx.query_all(statement("SELECT p.id FROM performance_participant p JOIN review_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.review_cycle_id WHERE p.tenant_id=$1 AND p.review_cycle_id=$2 AND NOT p.is_excluded AND c.status='ACTIVE' AND c.current_stage IN ('GOAL_SETTING','MANAGER_REVIEW') ORDER BY p.id",vec![tenant.into(),cycle.into()])).await?;
    let mut count = 0;
    for row in rows {
        count += emit_performance_participant(
            tx,
            metadata,
            tenant,
            row.try_get("", "id")?,
            actor.clone(),
            &[],
            PerformanceEmailChange::Action,
        )
        .await?;
    }
    Ok(count)
}
pub async fn emit_performance_participant(
    tx: &DatabaseTransaction,
    metadata: &DatabaseConnection,
    tenant: Uuid,
    id: Uuid,
    actor: Option<EmailActorRef>,
    aliases: &[Uuid],
    change: PerformanceEmailChange,
) -> KabiPayResult<usize> {
    let Some(state) = crate::email_alerts::current_state(tx, tenant, id).await? else {
        return Ok(0);
    };
    let (kind, key, at) = match change {
        PerformanceEmailChange::Action if state.pending => (
            EmailEventKind::ActionRequired,
            state.pending_key.clone(),
            state.action_at,
        ),
        PerformanceEmailChange::GoalsOutcome if state.goals_approved => (
            EmailEventKind::Outcome,
            state.goal_outcome_key.clone(),
            state.goal_outcome_at,
        ),
        PerformanceEmailChange::ReviewOutcome if state.manager_submitted => {
            let Some(at) = state.review_outcome_at else {
                return Ok(0);
            };
            (
                EmailEventKind::Outcome,
                state.review_outcome_key.clone(),
                at,
            )
        }
        _ => return Ok(0),
    };
    let transition = EmailTransition {
        tenant_id: tenant,
        resource_type: EmailResourceType::PerformanceReview,
        resource_id: id,
        resource_version: if kind == EmailEventKind::ActionRequired {
            state.action_version.clone()
        } else {
            key.clone()
        },
        occurred_at: at,
        actor_ref: actor,
        actor_target_user_ids: aliases.to_vec(),
    };
    if kind == EmailEventKind::Outcome {
        let Some(user) = state.requester_user_id else {
            return Ok(0);
        };
        return append_transition_emails(
            tx,
            &transition,
            kind,
            vec![local_transition_reviewer(&transition, user, &key)],
        )
        .await;
    }
    let mut reviewers = Vec::new();
    for user in
        select_all_scope_email_users(tx, tenant, state.employee_id, "performance:manage").await?
    {
        reviewers.push(local_transition_reviewer(&transition, user, &key));
    }
    // Evaluation follows the assigned review manager snapshot, not a broader
    // reporting-tree filter. One current exact permission check for that manager.
    if let Some(manager) = state.manager_employee_id {
        let rows=tx.query_all(statement("SELECT e.user_id FROM employee e JOIN \"user\" u ON u.tenant_id=e.tenant_id AND u.id=e.user_id WHERE e.tenant_id=$1 AND e.id=$2 AND NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND u.is_active AND NOT u.is_deleted",vec![tenant.into(),manager.into()])).await?;
        if rows.len() == 1 {
            let user = rows[0].try_get("", "user_id")?;
            if current_email_permission_scope(tx, tenant, user, "performance:evaluate").await?
                == Some(ScopeType::Team)
            {
                reviewers.push(local_transition_reviewer(&transition, user, &key));
            }
        }
    }
    if state.stage == "MANAGER_REVIEW" {
        let candidates = select_organization_email_reviewers(
            tx,
            metadata,
            tenant,
            state.employee_id,
            EmailResourceType::PerformanceReview,
            id,
        )
        .await?;
        // Resolve the single installed review assignment once, then compare exact
        // principal/assignment/version for every candidate without per-candidate SQL.
        let snapshot = if candidates.is_empty() {
            None
        } else {
            tx.query_one(statement("SELECT principal_id,assignment_id,assignment_version FROM organization_review_manager WHERE tenant_id=$1 AND participant_id=$2",vec![tenant.into(),id.into()])).await?
        };
        let current_assignment = if let Some(snapshot) = snapshot {
            let principal: Uuid = snapshot.try_get("", "principal_id")?;
            let assignment: Uuid = snapshot.try_get("", "assignment_id")?;
            let version: i64 = snapshot.try_get("", "assignment_version")?;
            if tx
                .query_one(statement(
                    crate::services::performance_management::REVIEW_MANAGER_ASSIGNMENT_SQL,
                    vec![
                        tenant.into(),
                        id.into(),
                        principal.into(),
                        assignment.into(),
                        version.into(),
                        state.employee_id.into(),
                    ],
                ))
                .await?
                .is_some()
            {
                Some((principal, assignment, version))
            } else {
                None
            }
        } else {
            None
        };
        for candidate in candidates {
            match &candidate.reviewer.authorization_ref {
                EmailAuthorizationRef::OrganizationGrant { .. } if candidate.scope == "ALL" => {}
                EmailAuthorizationRef::OrganizationRelationship {
                    relationship_id,
                    version,
                    ..
                } => {
                    let EmailRecipientRef::OrganizationPrincipal { principal_id, .. } =
                        candidate.reviewer.recipient
                    else {
                        continue;
                    };
                    if current_assignment != Some((principal_id, *relationship_id, *version)) {
                        continue;
                    }
                }
                _ => continue,
            }
            if let (
                Some(EmailActorRef::LocalUser { user_id }),
                EmailRecipientRef::OrganizationPrincipal {
                    source_tenant_id, ..
                },
            ) = (&transition.actor_ref, &candidate.reviewer.recipient)
            {
                if *source_tenant_id == tenant && *user_id == candidate.source_user_id {
                    continue;
                }
            }
            reviewers.push(candidate.reviewer);
        }
    }
    append_transition_emails(tx, &transition, kind, reviewers).await
}
