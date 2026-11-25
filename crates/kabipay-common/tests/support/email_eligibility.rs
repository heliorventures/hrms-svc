use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub fn request(resource: EmailResourceType) -> EmailEligibilityRequest {
    let now = Utc::now();
    let id = Uuid::new_v4();
    let category = match resource {
        EmailResourceType::Leave => EmailCategory::Leave,
        _ => EmailCategory::Approval,
    };
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        resource_type: resource,
        resource_id: id,
        resource_version: "APPROVED:123".into(),
        category,
        kind: EmailEventKind::Outcome,
        template_version: 1,
        recipient: EmailRecipientRef::LocalUser {
            user_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: resource,
            transition_key: "approved".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: resource,
            resource_id: id,
        },
        occurred_at: now,
        expires_at: now + Duration::days(7),
    };
    EmailEligibilityRequest {
        intent_id: Uuid::new_v4(),
        event_hash: event.canonical_hash().unwrap(),
        event,
    }
}
#[derive(Debug)]
struct Empty {
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Empty {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        Ok(vec![])
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom(
            "eligibility must not write business data".into(),
        ))
    }
}
pub async fn empty_db() -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Empty {
            queries: queries.clone(),
        })),
    )
    .await
    .unwrap();
    (db, queries)
}
