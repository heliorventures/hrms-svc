//! Read-only settings transition preview; scheduling revalidates under its lock.
use super::*;
use chrono::{DateTime, Utc};
use kabipay_common::{context::ClientClaims, tenant_business_clock::TenantBusinessClock, KabiPayResult};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct AttendanceDayPolicyPreview {
    pub revision: i64,
    pub transition: AttendanceDayWindow,
    pub following: AttendanceDayWindow,
}

pub async fn preview_policy<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock, claims: &ClientClaims,
    command: SchedulePolicyCommand, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayPolicyPreview> {
    super::repository::validate_schedule_request(tenant_id, claims, &command)?;
    let state = policy(db, tenant_id, clock, now).await?;
    let revision = state.revision;
    let proposal = super::repository::propose_policy_change(db, tenant_id, clock, state, &command, now).await?;
    let mut transition = proposal.transition;
    let mut following = proposal.following;
    // A preview does not persist or reserve these prospective policy versions.
    transition.policy_version_id = Uuid::nil();
    following.policy_version_id = Uuid::nil();
    Ok(AttendanceDayPolicyPreview { revision, transition, following })
}
