//! Transactional survey management lifecycle; never records participation.

use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{d0027_communication_audit::audit_log, d0076_anonymous_surveys::survey};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::resolvers::types::SurveyManagementEventDto;

/// Only management actions are representable. Do not add response/completion actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementAction { Saved, Published, Opened, ManuallyOpened, ManuallyClosed, AutomaticallyClosed, TransitionFailed }

impl ManagementAction {
    fn code(self) -> &'static str {
        match self {
            Self::Saved => "SURVEY_SAVED", Self::Published => "SURVEY_PUBLISHED",
            Self::Opened => "SURVEY_OPENED", Self::ManuallyClosed => "SURVEY_MANUALLY_CLOSED",
            Self::ManuallyOpened => "SURVEY_MANUALLY_OPENED",
            Self::AutomaticallyClosed => "SURVEY_AUTOMATICALLY_CLOSED", Self::TransitionFailed => "SURVEY_TRANSITION_FAILED",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Saved => "Survey draft saved.",
            Self::Published => "Survey published; audience frozen at publication.",
            Self::Opened => "Survey opening recorded by the scheduler.",
            Self::ManuallyOpened => "Scheduled survey opened early by an administrator; publication audience unchanged.",
            Self::ManuallyClosed => "Survey closed by an administrator.",
            Self::AutomaticallyClosed => "Survey closed at or after its scheduled deadline.",
            Self::TransitionFailed => "Scheduled transition failed. The worker will retry; contact an administrator if it persists.",
        }
    }
    fn from_code(code: &str) -> Option<Self> {
        [Self::Saved, Self::Published, Self::Opened, Self::ManuallyOpened, Self::ManuallyClosed, Self::AutomaticallyClosed, Self::TransitionFailed]
            .into_iter().find(|action| action.code() == code)
    }
}

/// Reject an already elapsed deadline using a clock sampled after the survey lock.
pub fn validate_publication(closes_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> KabiPayResult<()> {
    if closes_at.is_some_and(|closes| closes <= now) {
        return Err(KabiPayError::Validation("Survey close time has elapsed; update the draft schedule before publishing".into()));
    }
    Ok(())
}

fn validate_manual_open(status: &str, opens_at: Option<DateTime<Utc>>, closes_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> KabiPayResult<()> {
    if status != "PUBLISHED" || !opens_at.is_some_and(|opens| opens > now) {
        return Err(KabiPayError::Validation("Only a published survey with a future opening time can be opened early".into()));
    }
    validate_publication(closes_at, now)
}

/// Open a published schedule early without changing its questionnaire or frozen audience.
pub async fn open_survey(db: &DatabaseConnection, tenant_id: Uuid, survey_id: Uuid, actor: Uuid) -> KabiPayResult<()> {
    open_survey_with_clock(db, tenant_id, survey_id, actor, Utc::now).await
}

pub(crate) async fn open_survey_with_clock<F: FnOnce() -> DateTime<Utc>>(db: &DatabaseConnection, tenant_id: Uuid, survey_id: Uuid, actor: Uuid, clock: F) -> KabiPayResult<()> {
    let txn = db.begin().await?;
    let row = survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive().one(&txn).await?
        .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() })?;
    let now = clock();
    validate_manual_open(&row.status, row.opens_at, row.closes_at, now)?;
    survey::Entity::update_many().col_expr(survey::Column::OpensAt, sea_orm::sea_query::Expr::value(now))
        .col_expr(survey::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
        .filter(survey::Column::TenantId.eq(tenant_id)).filter(survey::Column::Id.eq(survey_id)).exec(&txn).await?;
    record_management_event(&txn, tenant_id, survey_id, Some(actor), ManagementAction::ManuallyOpened, now).await?;
    txn.commit().await?;
    Ok(())
}

fn due_action(status: &str, opens_at: Option<DateTime<Utc>>, closes_at: Option<DateTime<Utc>>, opened: bool, now: DateTime<Utc>) -> Option<ManagementAction> {
    if status != "PUBLISHED" { return None; }
    if closes_at.is_some_and(|closes| closes <= now) { return Some(ManagementAction::AutomaticallyClosed); }
    if !opened && opens_at.is_none_or(|opens| opens <= now) { return Some(ManagementAction::Opened); }
    None
}

/// Append static management metadata in the caller's transaction. No arbitrary payload is accepted.
pub async fn record_management_event<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid, actor: Option<Uuid>, action: ManagementAction, occurred_at: DateTime<Utc>) -> KabiPayResult<()> {
    audit_log::Entity::insert(audit_log::ActiveModel {
        id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), user_id: Set(actor),
        entity_type: Set("SURVEY".into()), entity_id: Set(Some(survey_id)), action: Set(action.code().into()),
        before_state: Set(None), after_state: Set(None), ip_address: Set(None), user_agent: Set(None), created_at: Set(occurred_at),
    }).exec_without_returning(db).await?;
    Ok(())
}

fn management_events(tenant_id: Uuid, survey_id: Uuid) -> sea_orm::Select<audit_log::Entity> {
    audit_log::Entity::find().filter(audit_log::Column::TenantId.eq(tenant_id))
        .filter(audit_log::Column::EntityType.eq("SURVEY")).filter(audit_log::Column::EntityId.eq(survey_id))
}

/// Load safe management history only after the resolver has enforced manage=ALL.
pub async fn load_management_events<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid) -> KabiPayResult<Vec<SurveyManagementEventDto>> {
    survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).one(db).await?
        .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() })?;
    let rows = management_events(tenant_id, survey_id)
        .filter(audit_log::Column::Action.is_in(["SURVEY_SAVED", "SURVEY_PUBLISHED", "SURVEY_OPENED", "SURVEY_MANUALLY_OPENED", "SURVEY_MANUALLY_CLOSED", "SURVEY_AUTOMATICALLY_CLOSED", "SURVEY_TRANSITION_FAILED"]))
        .order_by_desc(audit_log::Column::CreatedAt).order_by_desc(audit_log::Column::Id).limit(100).all(db).await?;
    Ok(rows.into_iter().filter_map(|row| ManagementAction::from_code(&row.action).map(|action| SurveyManagementEventDto {
        action: action.code().into(), occurred_at: row.created_at, message: action.message().into(),
    })).collect())
}

pub(crate) async fn process_survey_with_clock<F: FnOnce() -> DateTime<Utc>>(db: &DatabaseConnection, tenant_id: Uuid, survey_id: Uuid, clock: F) -> KabiPayResult<Option<ManagementAction>> {
    let txn = db.begin().await?;
    let row = survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive().one(&txn).await?;
    let now = clock();
    let Some(row) = row else { txn.commit().await?; return Ok(None); };
    if row.status != "PUBLISHED" { txn.commit().await?; return Ok(None); }
    let opened = management_events(tenant_id, survey_id).filter(audit_log::Column::Action.is_in(["SURVEY_OPENED", "SURVEY_MANUALLY_OPENED"]))
        .select_only().column(audit_log::Column::Action).into_tuple::<String>().one(&txn).await?.is_some();
    let action = due_action(&row.status, row.opens_at, row.closes_at, opened, now);
    if let Some(action) = action {
        if action == ManagementAction::AutomaticallyClosed {
            survey::Entity::update_many().col_expr(survey::Column::Status, sea_orm::sea_query::Expr::value("CLOSED"))
                .col_expr(survey::Column::ClosedAt, sea_orm::sea_query::Expr::value(now))
                .col_expr(survey::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
                .filter(survey::Column::TenantId.eq(tenant_id)).filter(survey::Column::Id.eq(survey_id)).exec(&txn).await?;
        }
        record_management_event(&txn, tenant_id, survey_id, None, action, now).await?;
    }
    txn.commit().await?;
    Ok(action)
}

/// A failed attempt is recorded once until a successful transition supersedes it.
/// Uses a fresh transaction because the failed transition transaction must roll back.
pub(crate) async fn record_transition_failure(db: &DatabaseConnection, tenant_id: Uuid, survey_id: Uuid) -> KabiPayResult<()> {
    let txn = db.begin().await?;
    let row = survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive().one(&txn).await?;
    if row.is_some_and(|row| row.status == "PUBLISHED") {
        let latest = management_events(tenant_id, survey_id)
            .filter(audit_log::Column::Action.is_in(["SURVEY_OPENED", "SURVEY_MANUALLY_OPENED", "SURVEY_AUTOMATICALLY_CLOSED", "SURVEY_TRANSITION_FAILED"]))
            .order_by_desc(audit_log::Column::CreatedAt).order_by_desc(audit_log::Column::Id)
            .select_only().column(audit_log::Column::Action).into_tuple::<String>().one(&txn).await?;
        if latest.as_deref() != Some("SURVEY_TRANSITION_FAILED") {
            record_management_event(&txn, tenant_id, survey_id, None, ManagementAction::TransitionFailed, Utc::now()).await?;
        }
    }
    txn.commit().await?;
    Ok(())
}

#[derive(Default, Debug)]
pub struct SurveySweepResult { pub opened: usize, pub closed: usize, pub failed: usize }

/// Called by the tenant worker only after current survey-module entitlement succeeds.
/// Keyset pages prevent unchanged/open or failing surveys from starving later due work.
pub async fn process_due_surveys(db: &DatabaseConnection, tenant_id: Uuid) -> KabiPayResult<SurveySweepResult> {
    let mut result = SurveySweepResult::default();
    let mut cursor: Option<Uuid> = None;
    loop {
        let mut query = survey::Entity::find().filter(survey::Column::TenantId.eq(tenant_id)).filter(survey::Column::Status.eq("PUBLISHED"));
        if let Some(cursor) = cursor { query = query.filter(survey::Column::Id.gt(cursor)); }
        let ids = query.order_by_asc(survey::Column::Id).limit(100).select_only().column(survey::Column::Id).into_tuple::<Uuid>().all(db).await?;
        if ids.is_empty() { break; }
        cursor = ids.last().copied();
        let page_len = ids.len();
        for survey_id in ids {
            match process_survey_with_clock(db, tenant_id, survey_id, Utc::now).await {
                Ok(Some(ManagementAction::Opened)) => result.opened += 1,
                Ok(Some(ManagementAction::AutomaticallyClosed)) => result.closed += 1,
                Ok(_) => {}
                Err(error) => {
                    result.failed += 1;
                    tracing::error!(%tenant_id, %survey_id, code = error.code(), "scheduled survey transition failed; will retry");
                    if let Err(error) = record_transition_failure(db, tenant_id, survey_id).await {
                        tracing::error!(%tenant_id, %survey_id, code = error.code(), "survey transition exception recording failed");
                    }
                }
            }
        }
        if page_len < 100 { break; }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn elapsed_publication_is_rejected_at_exclusive_boundary() {
        let now = Utc::now();
        assert!(validate_publication(Some(now), now).is_err());
        assert!(validate_publication(Some(now - Duration::seconds(1)), now).is_err());
        assert!(validate_publication(Some(now + Duration::seconds(1)), now).is_ok());
        assert!(validate_publication(None, now).is_ok());
    }

    #[test]
    fn manual_open_requires_scheduled_published_survey_and_unelapsed_close() {
        let now = Utc::now();
        let future = Some(now + Duration::hours(1));
        assert!(validate_manual_open("PUBLISHED", future, None, now).is_ok());
        assert!(validate_manual_open("PUBLISHED", future, Some(now), now).is_err());
        assert!(validate_manual_open("PUBLISHED", None, None, now).is_err());
        assert!(validate_manual_open("PUBLISHED", Some(now), None, now).is_err());
        assert!(validate_manual_open("DRAFT", future, None, now).is_err());
        assert!(validate_manual_open("CLOSED", future, None, now).is_err());
    }

    #[test]
    fn scheduled_transition_respects_future_open_and_exclusive_close() {
        let now = Utc::now();
        assert_eq!(due_action("DRAFT", None, None, false, now), None);
        assert_eq!(due_action("PUBLISHED", Some(now + Duration::seconds(1)), None, false, now), None);
        assert_eq!(due_action("PUBLISHED", Some(now), None, false, now), Some(ManagementAction::Opened));
        assert_eq!(due_action("PUBLISHED", None, None, true, now), None);
        assert_eq!(due_action("PUBLISHED", None, Some(now), false, now), Some(ManagementAction::AutomaticallyClosed));
        assert_eq!(due_action("CLOSED", None, Some(now), true, now), None);
    }

    #[tokio::test]
    async fn management_operations_reject_results_and_narrow_manage_permissions_before_database_access() {
        for scope in ["TEAM", "DEPARTMENT", "SELF"] {
            let claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
                "sub": uuid::Uuid::new_v4(), "tenant_id": uuid::Uuid::new_v4(), "iss": "kabipay-client",
                "iat": 0, "exp": 9999999999i64, "permissions": ["survey:manage", "survey:results"],
                "permission_scopes": {"survey:manage": scope, "survey:results": "ALL"}
            })).unwrap();
            let schema = async_graphql::Schema::build(crate::resolvers::QueryRoot, crate::resolvers::MutationRoot, async_graphql::EmptySubscription)
                .data(claims).finish();
            let response = schema.execute(format!("{{ surveyManagementEvents(surveyId: \"{}\") {{ action occurredAt message }} }}", uuid::Uuid::new_v4())).await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("ALL scope required"));
            let response = schema.execute(format!("mutation {{ openSurvey(surveyId: \"{}\") {{ summary {{ id }} }} }}", uuid::Uuid::new_v4())).await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("ALL scope required"));
        }
    }
}
