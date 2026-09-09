//! Mutations for subscriptions, feature flags, module catalog (ops plane).

use chrono::{NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::{feature_flag, module, tenant, tenant_subscription};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

const SUB_STATUSES: &[&str] = &["PENDING", "ACTIVE", "SUSPENDED", "CANCELLED", "EXPIRED"];
const OVERAGE: &[&str] = &["BLOCK", "ALLOW", "NOTIFY"];

pub async fn upsert_tenant_subscription(
    db: &DatabaseConnection,
    operator_user_id: Uuid,
    tenant_id: Uuid,
    module_id: Uuid,
    status: String,
    contracted_seats: i32,
    overage_policy: String,
    activated_at: Option<NaiveDate>,
    expires_at: Option<NaiveDate>,
) -> KabiPayResult<tenant_subscription::Model> {
    if !SUB_STATUSES.contains(&status.as_str()) {
        return Err(KabiPayError::Validation(format!(
            "invalid subscription status {status}"
        )));
    }
    if !OVERAGE.contains(&overage_policy.as_str()) {
        return Err(KabiPayError::Validation(format!(
            "invalid overage_policy {overage_policy}"
        )));
    }
    if contracted_seats < 0 {
        return Err(KabiPayError::Validation(
            "contracted_seats must be non-negative".into(),
        ));
    }

    if matches!((activated_at, expires_at), (Some(start), Some(end)) if start >= end) {
        return Err(KabiPayError::Validation("subscription expiry must be after activation".into()));
    }
    let txn = db.begin().await?;

    tenant::Entity::find_by_id(tenant_id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tenant",
            id: tenant_id.to_string(),
        })?;

    module::Entity::find_by_id(module_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "module",
            id: module_id.to_string(),
        })?;

    let existing = tenant_subscription::Entity::find()
        .filter(tenant_subscription::Column::TenantId.eq(tenant_id))
        .filter(tenant_subscription::Column::ModuleId.eq(module_id))
        .filter(tenant_subscription::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(&txn)
        .await?;

    let now = Utc::now();
    if let Some(mut row) = existing {
        row.contracted_seats = contracted_seats;
        row.overage_policy = overage_policy.clone();
        enforce_seat_cap(&row)?;
        let mut am: tenant_subscription::ActiveModel = row.into();
        am.status = Set(status);
        am.contracted_seats = Set(contracted_seats);
        am.overage_policy = Set(overage_policy);
        am.activated_at = Set(activated_at);
        am.expires_at = Set(expires_at);
        am.approved_by = Set(Some(operator_user_id));
        am.updated_at = Set(now);
        let m = am.update(&txn).await?;
        txn.commit().await?;
        return Ok(m);
    }

    let m = tenant_subscription::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        module_id: Set(module_id),
        status: Set(status),
        activated_at: Set(activated_at),
        expires_at: Set(expires_at),
        contracted_seats: Set(contracted_seats),
        current_seat_usage: Set(0),
        overage_policy: Set(overage_policy),
        approved_by: Set(Some(operator_user_id)),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok(m)
}

fn enforce_seat_cap(m: &tenant_subscription::Model) -> KabiPayResult<()> {
    if m.current_seat_usage > m.contracted_seats && m.overage_policy == "BLOCK" {
        return Err(KabiPayError::SeatLimitReached {
            module_code: m.module_id.to_string(),
            contracted: m.contracted_seats,
            current: m.current_seat_usage,
        });
    }
    Ok(())
}

#[cfg(test)]
mod subscription_security_tests {
    use super::*;

    #[tokio::test]
    async fn rejected_seat_reduction_never_emits_an_update() {
        use sea_orm::entity::prelude::async_trait;
        use sea_orm::{Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
        use std::{collections::{BTreeMap, VecDeque}, sync::{Arc, Mutex}};
        #[derive(Clone, Debug)]
        struct Proxy {
            replies: Arc<Mutex<VecDeque<Vec<ProxyRow>>>>,
            statements: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl ProxyDatabaseTrait for Proxy {
            async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
                self.statements.lock().unwrap().push(statement.to_string());
                self.replies.lock().unwrap().pop_front().ok_or_else(|| DbErr::Custom("unexpected write/query".into()))
            }
            async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
                panic!("rejected subscription must not execute writes: {statement}")
            }
        }
        let tid = Uuid::new_v4();
        let mid = Uuid::new_v4();
        let now = Utc::now();
        let mut tenant_fields = BTreeMap::from([
            ("id".into(), tid.into()), ("name".into(), "test".into()),
            ("status".into(), "ACTIVE".into()), ("is_deleted".into(), false.into()),
            ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
            ("account_manager_id".into(), Option::<Uuid>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
        ]);
        for name in ["plan", "country", "timezone", "currency", "gstin", "pan", "registered_address", "logo_url", "primary_color", "subdomain"] {
            tenant_fields.insert(name.into(), Option::<String>::None.into());
        }
        let module_row = ProxyRow::new(BTreeMap::from([
            ("id".into(), mid.into()), ("code".into(), "LEAVE".into()), ("name".into(), "Leave".into()),
            ("category".into(), Option::<String>::None.into()), ("description".into(), Option::<String>::None.into()),
            ("is_active".into(), true.into()), ("is_core".into(), false.into()), ("display_order".into(), 1.into()),
            ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
        ]));
        let subscription = ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::new_v4().into()), ("tenant_id".into(), tid.into()), ("module_id".into(), mid.into()),
            ("status".into(), "ACTIVE".into()), ("activated_at".into(), Option::<NaiveDate>::None.into()),
            ("expires_at".into(), Option::<NaiveDate>::None.into()), ("contracted_seats".into(), 10.into()),
            ("current_seat_usage".into(), 10.into()), ("overage_policy".into(), "BLOCK".into()),
            ("approved_by".into(), Option::<Uuid>::None.into()), ("is_deleted".into(), false.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
        ]));
        let proxy = Proxy {
            replies: Arc::new(Mutex::new(VecDeque::from([vec![ProxyRow::new(tenant_fields)], vec![module_row], vec![subscription]]))),
            statements: Arc::new(Mutex::new(Vec::new())),
        };
        let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy.clone()))).await.unwrap();
        let result = upsert_tenant_subscription(&db, Uuid::new_v4(), tid, mid, "ACTIVE".into(), 9, "BLOCK".into(), None, None).await;
        assert!(matches!(result, Err(KabiPayError::SeatLimitReached { .. })), "{result:?}");
        let statements = proxy.statements.lock().unwrap();
        assert_eq!(statements.len(), 3);
        assert!(statements.iter().all(|sql| sql.starts_with("SELECT")));
        assert!(statements[0].ends_with("FOR UPDATE"));
        assert!(statements[2].ends_with("FOR UPDATE"));
        assert!(statements[2].contains(&tid.to_string()) && statements[2].contains(&mid.to_string()));
    }

    #[tokio::test]
    async fn invalid_date_window_is_rejected_without_database_access() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        for end in [start, start.pred_opt().unwrap()] {
            let result = upsert_tenant_subscription(
                &DatabaseConnection::Disconnected, Uuid::nil(), Uuid::nil(), Uuid::nil(),
                "ACTIVE".into(), 10, "BLOCK".into(), Some(start), Some(end),
            ).await;
            assert!(matches!(result, Err(KabiPayError::Validation(_))));
        }
    }

    #[test]
    fn prospective_seat_cap_respects_block_allow_notify_and_exact_capacity() {
        let now = Utc::now();
        let mut row = tenant_subscription::Model {
            id: Uuid::nil(), tenant_id: Uuid::nil(), module_id: Uuid::nil(),
            status: "ACTIVE".into(), activated_at: None, expires_at: None,
            contracted_seats: 10, current_seat_usage: 10, overage_policy: "BLOCK".into(),
            approved_by: None, is_deleted: false, deleted_at: None, deleted_by: None,
            created_at: now, updated_at: now,
        };
        assert!(enforce_seat_cap(&row).is_ok());
        row.contracted_seats = 9;
        assert!(matches!(enforce_seat_cap(&row), Err(KabiPayError::SeatLimitReached { .. })));
        for policy in ["ALLOW", "NOTIFY"] {
            row.overage_policy = policy.into();
            assert!(enforce_seat_cap(&row).is_ok());
        }
    }
}

pub async fn remove_tenant_subscription(
    db: &DatabaseConnection,
    operator_user_id: Uuid,
    subscription_id: Uuid,
) -> KabiPayResult<bool> {
    let row = tenant_subscription::Entity::find_by_id(subscription_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tenant_subscription",
            id: subscription_id.to_string(),
        })?;
    if row.is_deleted {
        return Ok(false);
    }
    let now = Utc::now();
    let mut am: tenant_subscription::ActiveModel = row.into();
    am.is_deleted = Set(true);
    am.deleted_at = Set(Some(now));
    am.deleted_by = Set(Some(operator_user_id));
    am.status = Set("CANCELLED".into());
    am.updated_at = Set(now);
    am.update(db).await?;
    Ok(true)
}

pub async fn upsert_feature_flag(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    feature_name: String,
    is_enabled: bool,
) -> KabiPayResult<feature_flag::Model> {
    let fname = feature_name.trim();
    if fname.is_empty() || fname.len() > 255 {
        return Err(KabiPayError::Validation(
            "feature_name must be 1–255 characters".into(),
        ));
    }
    let fname = fname.to_string();

    tenant::Entity::find_by_id(tenant_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tenant",
            id: tenant_id.to_string(),
        })?;

    let existing = feature_flag::Entity::find()
        .filter(feature_flag::Column::TenantId.eq(tenant_id))
        .filter(feature_flag::Column::FeatureName.eq(fname.clone()))
        .one(db)
        .await?;

    let now = Utc::now();
    if let Some(row) = existing {
        let mut am: feature_flag::ActiveModel = row.into();
        am.is_enabled = Set(is_enabled);
        am.updated_at = Set(now);
        return Ok(am.update(db).await?);
    }

    Ok(
        feature_flag::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            feature_name: Set(fname),
            is_enabled: Set(is_enabled),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?,
    )
}

pub async fn set_module_active(
    db: &DatabaseConnection,
    module_id: Uuid,
    is_active: bool,
) -> KabiPayResult<module::Model> {
    let row = module::Entity::find_by_id(module_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "module",
            id: module_id.to_string(),
        })?;
    let mut am: module::ActiveModel = row.into();
    am.is_active = Set(is_active);
    am.updated_at = Set(Utc::now());
    Ok(am.update(db).await?)
}

pub async fn update_tenant_fields(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    name: Option<String>,
    status: Option<String>,
    plan: Option<String>,
) -> KabiPayResult<tenant::Model> {
    const STATUSES: &[&str] = &["PROVISIONING", "ACTIVE", "SUSPENDED", "TERMINATED"];
    if let Some(ref s) = status {
        if !STATUSES.contains(&s.as_str()) {
            return Err(KabiPayError::Validation(format!("invalid tenant status {s}")));
        }
    }

    let row = tenant::Entity::find_by_id(tenant_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tenant",
            id: tenant_id.to_string(),
        })?;

    let mut am: tenant::ActiveModel = row.into();
    if let Some(n) = name {
        if n.trim().is_empty() {
            return Err(KabiPayError::Validation("name must not be empty".into()));
        }
        am.name = Set(n);
    }
    if let Some(s) = status {
        am.status = Set(s);
    }
    if let Some(p) = plan {
        am.plan = Set(Some(p));
    }
    am.updated_at = Set(Utc::now());
    Ok(am.update(db).await?)
}

pub async fn list_feature_flags(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<feature_flag::Model>> {
    let limit = limit.clamp(1, 500);
    Ok(feature_flag::Entity::find()
        .filter(feature_flag::Column::TenantId.eq(tenant_id))
        .order_by_asc(feature_flag::Column::FeatureName)
        .limit(limit)
        .all(db)
        .await?)
}
