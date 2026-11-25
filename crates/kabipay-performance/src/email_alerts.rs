//! Assigned goal-set/manager-review actions, never cycle-wide announcements.
//! No answers, ratings, comments or performance values enter an email intent.
use crate::services::{performance_lifecycle, performance_management};
use kabipay_common::{context::ScopeType, important_email::*, KabiPayError, KabiPayResult};
use kabipay_organization_tenant::{
    email_policy::current_email_organization_policy,
    email_routes::EmailDomainAdapter,
    email_workflow::{action_decision, require_email_installation},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone)]
pub struct PerformanceEmailAdapter;
impl EmailDomainAdapter for PerformanceEmailAdapter {
    fn domain(&self) -> &'static str {
        "performance"
    }
    async fn check_email_eligibility(
        &self,
        db: &DatabaseConnection,
        ops: &DatabaseConnection,
        request: &EmailEligibilityRequest,
    ) -> KabiPayResult<EmailEligibilityDecision> {
        check_email_eligibility(db, ops, request).await
    }
}
pub const EMAIL_RESOURCE_SQL:&str="SELECT p.id,p.employee_id,p.review_cycle_id,p.manager_employee_id,p.response_revision,p.updated_at,(p.self_submitted_at IS NOT NULL) AS self_submitted,(p.manager_submitted_at IS NOT NULL) AS manager_submitted,p.manager_submitted_at,c.current_stage,c.status AS cycle_status,c.updated_at AS cycle_updated_at,e.user_id AS requester_user_id FROM performance_participant p JOIN review_cycle c ON c.id=p.review_cycle_id AND c.tenant_id=p.tenant_id JOIN employee e ON e.id=p.employee_id AND e.tenant_id=p.tenant_id AND NOT e.is_deleted WHERE p.tenant_id=$1 AND p.id=$2 AND NOT p.is_excluded";
pub const EMAIL_GOALS_SQL:&str="SELECT id,status,weightage,updated_at FROM goal WHERE tenant_id=$1 AND review_cycle_id=$2 AND employee_id=$3 ORDER BY id LIMIT 1001";

pub struct PerformanceEmailState {
    pub participant_id: Uuid,
    pub action_at: chrono::DateTime<chrono::Utc>,
    pub goal_outcome_at: chrono::DateTime<chrono::Utc>,
    pub review_outcome_at: Option<chrono::DateTime<chrono::Utc>>,
    pub employee_id: Uuid,
    pub requester_user_id: Option<Uuid>,
    pub manager_employee_id: Option<Uuid>,
    pub stage: String,
    pub revision: i32,
    pub action_version: String,
    pub pending_key: String,
    pub pending: bool,
    pub goal_outcome_key: String,
    pub goals_approved: bool,
    pub review_outcome_key: String,
    pub manager_submitted: bool,
}

pub async fn current_state(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    participant: Uuid,
) -> KabiPayResult<Option<PerformanceEmailState>> {
    let Some(row) = db
        .query_one(statement(
            EMAIL_RESOURCE_SQL,
            vec![tenant.into(), participant.into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let employee: Uuid = row.try_get("", "employee_id")?;
    let cycle: Uuid = row.try_get("", "review_cycle_id")?;
    let goals = db
        .query_all(statement(
            EMAIL_GOALS_SQL,
            vec![tenant.into(), cycle.into(), employee.into()],
        ))
        .await?;
    if goals.len() > 1000 {
        return Err(KabiPayError::Validation("EMAIL_REVIEW_GOAL_LIMIT".into()));
    }
    let mut metadata = Vec::with_capacity(goals.len());
    let mut weights = Vec::with_capacity(goals.len());
    let mut all_approved = !goals.is_empty();
    let mut goal_at: Option<chrono::DateTime<chrono::Utc>>=None;
    for goal in &goals {
        let id: Uuid = goal.try_get("", "id")?;
        let status: String = goal.try_get("", "status")?;
        let weight: Option<rust_decimal::Decimal> = goal.try_get("", "weightage")?;
        let at: chrono::DateTime<chrono::Utc> = goal.try_get("", "updated_at")?;
        goal_at=Some(goal_at.map_or(at,|old|old.max(at)));
        all_approved &= status == "APPROVED";
        if let Some(weight) = weight {
            weights.push(weight);
        }
        metadata.push((
            id,
            status,
            weight.map(|value| value.to_string()),
            at.timestamp_micros(),
        ));
    }
    let goal_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&metadata)?));
    let weights_valid = weights.len() == goals.len()
        && performance_lifecycle::validate_goal_weight_total(&weights).is_ok();
    let stage: String = row.try_get("", "current_stage")?;
    let active: String = row.try_get("", "cycle_status")?;
    let revision: i32 = row.try_get("", "response_revision")?;
    let self_submitted: bool = row.try_get("", "self_submitted")?;
    let manager_submitted: bool = row.try_get("", "manager_submitted")?;
    let pending = active == "ACTIVE"
        && match stage.as_str() {
            "GOAL_SETTING" => !all_approved && weights_valid,
            "MANAGER_REVIEW" => self_submitted && !manager_submitted,
            _ => false,
        };
    let at: chrono::DateTime<chrono::Utc> = row.try_get("", "updated_at")?;
    let cycle_at: chrono::DateTime<chrono::Utc> = row.try_get("", "cycle_updated_at")?;
    let version = format!(
        "{}:{}:{}:{}:{}",
        stage,
        revision,
        at.timestamp_micros(),
        cycle_at.timestamp_micros(),
        goal_digest
    );
    let manager = performance_management::effective_review_manager(
        db,
        tenant,
        participant,
        employee,
        row.try_get("", "manager_employee_id")?,
    )
    .await?;
    Ok(Some(PerformanceEmailState {
        participant_id: participant,
        action_at: at.max(cycle_at).max(goal_at.unwrap_or(at)),
        goal_outcome_at: goal_at.unwrap_or(at),
        review_outcome_at: row.try_get("", "manager_submitted_at")?,
        employee_id: employee,
        requester_user_id: row.try_get("", "requester_user_id")?,
        manager_employee_id: manager,
        pending_key: format!(
            "{}:{}",
            if stage == "GOAL_SETTING" {
                "goals_pending"
            } else {
                "manager_review"
            },
            revision
        ),
        stage,
        revision,
        action_version: version,
        pending,
        goal_outcome_key: format!("goals_approved:{revision}:{goal_digest}"),
        goals_approved: all_approved,
        review_outcome_key: format!("manager_submitted:{revision}"),
        manager_submitted,
    }))
}

pub async fn check_email_eligibility(
    db: &DatabaseConnection,
    _ops: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    if event.resource_type != EmailResourceType::PerformanceReview {
        return Ok(EmailEligibilityDecision::Forbidden);
    }
    let Some(state) = current_state(db, event.tenant_id, event.resource_id).await? else {
        return Ok(EmailEligibilityDecision::Obsolete);
    };
    let subject = EmailSubjectSnapshot {
        employee_id: state.employee_id,
        requester_user_id: state.requester_user_id,
        resource_version: state.action_version.clone(),
        pending: state.pending,
        outcome_is_current: (state.goals_approved
            && email_outcome_matches(event, &state.goal_outcome_key))
            || (state.manager_submitted && email_outcome_matches(event, &state.review_outcome_key)),
    };
    match check_email_subject(event, &subject) {
        EmailSubjectCheck::RequesterOutcome => return Ok(EmailEligibilityDecision::Eligible),
        EmailSubjectCheck::Obsolete => return Ok(EmailEligibilityDecision::Obsolete),
        EmailSubjectCheck::CheckCurrentActionAuthority => {}
        _ => return Ok(EmailEligibilityDecision::Forbidden),
    }
    match event.recipient {
        EmailRecipientRef::LocalUser { user_id } => {
            if !email_outcome_matches(event, &state.pending_key) {
                return Ok(EmailEligibilityDecision::Forbidden);
            }
            if current_email_permission_scope(db, event.tenant_id, user_id, "performance:manage")
                .await?
                == Some(ScopeType::All)
            {
                return Ok(EmailEligibilityDecision::Eligible);
            }
            if current_email_permission_scope(db, event.tenant_id, user_id, "performance:evaluate")
                .await?
                != Some(ScopeType::Team)
            {
                return Ok(EmailEligibilityDecision::Forbidden);
            }
            let Some(actor) = current_email_actor(db, event.tenant_id, user_id).await? else {
                return Ok(EmailEligibilityDecision::Forbidden);
            };
            Ok(
                if performance_lifecycle::manager_matches_snapshot(
                    actor.employee_id,
                    state.manager_employee_id,
                ) {
                    EmailEligibilityDecision::Eligible
                } else {
                    EmailEligibilityDecision::Forbidden
                },
            )
        }
        EmailRecipientRef::OrganizationPrincipal { principal_id, .. } => {
            // Existing organization ReviewSubmit acts only at manager review. It is not
            // a new cross-company goal-approval or self-appraisal API.
            if state.stage != "MANAGER_REVIEW" {
                return Ok(EmailEligibilityDecision::Forbidden);
            }
            let policy = current_email_organization_policy(&EmailOrganizationPolicyRequest {
                intent: request.clone(),
                subject_employee_id: state.employee_id,
            })
            .await?;
            require_email_installation(
                db,
                event,
                state.employee_id,
                principal_id,
                "performance:evaluate",
                &policy,
            )
            .await?;
            match event.authorization_ref {
                EmailAuthorizationRef::OrganizationGrant { .. } if policy.scope == "ALL" => {
                    Ok(EmailEligibilityDecision::Eligible)
                }
                EmailAuthorizationRef::OrganizationRelationship {
                    relationship_id,
                    version,
                    ..
                } => action_decision(
                    performance_management::assert_current_review_assignment(
                        db,
                        event.tenant_id,
                        state.participant_id,
                        principal_id,
                        relationship_id,
                        version,
                        state.employee_id,
                        false,
                    )
                    .await,
                ),
                _ => Ok(EmailEligibilityDecision::Forbidden),
            }
        }
    }
}
