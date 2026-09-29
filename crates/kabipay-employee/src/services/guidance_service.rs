//! Persistence for the authenticated user's application overview state.

use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0089_user_guidance_state::user_guidance_state;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect,
};
use uuid::Uuid;

/// Return the overview dismissal timestamp for this tenant/user pair, if present.
pub async fn load_state(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<Option<DateTime<Utc>>> {
    user_guidance_state::Entity::find()
        .select_only()
        .column(user_guidance_state::Column::OverviewDismissedAt)
        .filter(user_guidance_state::Column::TenantId.eq(tenant_id))
        .filter(user_guidance_state::Column::UserId.eq(user_id))
        .into_tuple::<DateTime<Utc>>()
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

/// Dismiss the overview once and return the database's persisted timestamp.
/// The unique tenant/user key and `DO NOTHING` conflict action make concurrent
/// calls converge on the original row without changing its dismissal time.
pub async fn dismiss_overview(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<DateTime<Utc>> {
    user_guidance_state::Entity::insert(user_guidance_state::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::columns([
            user_guidance_state::Column::TenantId,
            user_guidance_state::Column::UserId,
        ])
        .do_nothing()
        .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await
    .map_err(KabiPayError::from)?;

    load_state(db, tenant_id, user_id)
        .await?
        .ok_or_else(|| KabiPayError::Internal("guidance dismissal row missing after upsert".into()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use chrono::{DateTime, TimeZone, Utc};
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
        ProxyRow, Statement, Value,
    };
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    type StateRows = HashMap<(Uuid, Uuid), DateTime<Utc>>;

    #[derive(Clone, Debug)]
    pub(crate) struct GuidanceStore {
        rows: Arc<Mutex<StateRows>>,
        statements: Arc<Mutex<Vec<Statement>>>,
        write_rows_affected: Arc<Mutex<Vec<u64>>>,
    }

    impl GuidanceStore {
        pub(crate) fn row_count(&self) -> usize {
            self.rows.lock().expect("guidance state rows").len()
        }

        pub(crate) fn timestamp(&self, tenant_id: Uuid, user_id: Uuid) -> Option<DateTime<Utc>> {
            self.rows
                .lock()
                .expect("guidance state rows")
                .get(&(tenant_id, user_id))
                .copied()
        }

        pub(crate) fn statements(&self) -> Vec<Statement> {
            self.statements
                .lock()
                .expect("guidance statement recorder")
                .clone()
        }

        pub(crate) fn write_rows_affected(&self) -> Vec<u64> {
            self.write_rows_affected
                .lock()
                .expect("guidance write recorder")
                .clone()
        }
    }

    #[derive(Debug)]
    struct GuidanceProxy {
        known_tenants: Vec<Uuid>,
        known_users: Vec<Uuid>,
        rows: Arc<Mutex<StateRows>>,
        statements: Arc<Mutex<Vec<Statement>>>,
        write_rows_affected: Arc<Mutex<Vec<u64>>>,
        next_timestamp_seconds: AtomicI64,
    }

    impl GuidanceProxy {
        fn key_for_statement(&self, statement: &Statement) -> Result<(Uuid, Uuid), DbErr> {
            let values = statement
                .values
                .as_ref()
                .map(|values| values.0.as_slice())
                .unwrap_or(&[]);
            let tenant_matches: Vec<_> = self
                .known_tenants
                .iter()
                .copied()
                .filter(|tenant_id| values.contains(&Value::from(*tenant_id)))
                .collect();
            let user_matches: Vec<_> = self
                .known_users
                .iter()
                .copied()
                .filter(|user_id| values.contains(&Value::from(*user_id)))
                .collect();
            match (tenant_matches.as_slice(), user_matches.as_slice()) {
                ([tenant_id], [user_id]) => Ok((*tenant_id, *user_id)),
                _ => Err(DbErr::Custom(format!(
                    "guidance statement must bind one known tenant and user: {statement:?}"
                ))),
            }
        }
    }

    fn state_row(dismissed_at: DateTime<Utc>) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([(
            "overview_dismissed_at".into(),
            dismissed_at.into(),
        )]))
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for GuidanceProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("guidance statement recorder")
                .push(statement.clone());
            if !statement.sql.to_ascii_uppercase().contains("SELECT") {
                return Err(DbErr::Custom(format!(
                    "unexpected guidance query statement: {statement:?}"
                )));
            }
            let key = self.key_for_statement(&statement)?;
            Ok(self
                .rows
                .lock()
                .expect("guidance state rows")
                .get(&key)
                .copied()
                .map(state_row)
                .into_iter()
                .collect())
        }

        async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
            self.statements
                .lock()
                .expect("guidance statement recorder")
                .push(statement.clone());
            let sql = statement.sql.to_ascii_uppercase();
            if !sql.contains("INSERT INTO") {
                return Err(DbErr::Custom(format!(
                    "unexpected guidance write statement: {statement:?}"
                )));
            }
            let key = self.key_for_statement(&statement)?;
            let rows_affected = {
                let mut rows = self.rows.lock().expect("guidance state rows");
                if rows.contains_key(&key) {
                    if !sql.contains("ON CONFLICT") || !sql.contains("DO NOTHING") {
                        return Err(DbErr::Custom(format!(
                            "duplicate guidance key without DO NOTHING: {statement:?}"
                        )));
                    }
                    0
                } else {
                    let seconds = self.next_timestamp_seconds.fetch_add(1, Ordering::SeqCst);
                    let dismissed_at = Utc
                        .timestamp_opt(seconds, 0)
                        .single()
                        .ok_or_else(|| DbErr::Custom("invalid proxy timestamp".into()))?;
                    rows.insert(key, dismissed_at);
                    1
                }
            };
            self.write_rows_affected
                .lock()
                .expect("guidance write recorder")
                .push(rows_affected);
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected,
            })
        }
    }

    pub(crate) async fn connection(
        tenants: &[Uuid],
        users: &[Uuid],
        first_timestamp_seconds: i64,
    ) -> (DatabaseConnection, GuidanceStore) {
        let rows = Arc::new(Mutex::new(HashMap::new()));
        let statements = Arc::new(Mutex::new(Vec::new()));
        let write_rows_affected = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(GuidanceProxy {
                known_tenants: tenants.to_vec(),
                known_users: users.to_vec(),
                rows: Arc::clone(&rows),
                statements: Arc::clone(&statements),
                write_rows_affected: Arc::clone(&write_rows_affected),
                next_timestamp_seconds: AtomicI64::new(first_timestamp_seconds),
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (
            db,
            GuidanceStore {
                rows,
                statements,
                write_rows_affected,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use super::test_support::connection;
    use sea_orm::Statement;
    use uuid::Uuid;

    fn timestamp(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .expect("valid fixed timestamp")
    }

    fn values(statement: &Statement) -> &[sea_orm::Value] {
        statement.values.as_ref().map(|values| values.0.as_slice()).unwrap_or(&[])
    }

    #[tokio::test]
    async fn first_read_returns_no_dismissal_timestamp() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, store) = connection(&[tenant_id], &[user_id], 1_790_000_000).await;

        let state = load_state(&db, tenant_id, user_id)
            .await
            .expect("read guidance state");

        assert_eq!(state, None);
        assert_eq!(store.row_count(), 0);
    }

    #[tokio::test]
    async fn dismissal_returns_the_timestamp_stored_by_the_database() {
        let dismissed_at = timestamp(1_790_000_000);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, store) = connection(&[tenant_id], &[user_id], 1_790_000_000).await;

        let stored_at = dismiss_overview(&db, tenant_id, user_id)
            .await
            .expect("persist overview dismissal");

        assert_eq!(stored_at, dismissed_at);
        assert_eq!(store.row_count(), 1);
        assert_eq!(store.timestamp(tenant_id, user_id), Some(dismissed_at));
        let statements = store.statements();
        let write = statements
            .iter()
            .find(|statement| statement.sql.to_ascii_uppercase().contains("INSERT INTO"))
            .expect("guidance state insert");
        let write_sql = write.sql.to_ascii_uppercase();
        assert!(write_sql.contains("ON CONFLICT"));
        assert!(write_sql.contains("DO NOTHING"));
        let bound_values = values(write);
        assert!(bound_values.contains(&sea_orm::Value::from(tenant_id)));
        assert!(bound_values.contains(&sea_orm::Value::from(user_id)));
        let read = statements
            .iter()
            .find(|statement| statement.sql.to_ascii_uppercase().contains("SELECT"))
            .expect("select persisted guidance timestamp");
        let read_values = values(read);
        assert!(read_values.contains(&sea_orm::Value::from(tenant_id)));
        assert!(read_values.contains(&sea_orm::Value::from(user_id)));
    }

    #[tokio::test]
    async fn repeating_dismissal_keeps_the_original_timestamp_and_uses_conflict_handling() {
        let dismissed_at = timestamp(1_790_000_100);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, store) = connection(&[tenant_id], &[user_id], 1_790_000_100).await;

        let first = dismiss_overview(&db, tenant_id, user_id)
            .await
            .expect("first dismissal");
        let repeated = dismiss_overview(&db, tenant_id, user_id)
            .await
            .expect("repeated dismissal");

        assert_eq!(first, dismissed_at);
        assert_eq!(repeated, first);
        assert_eq!(store.row_count(), 1);
        assert_eq!(store.timestamp(tenant_id, user_id), Some(dismissed_at));
        assert_eq!(store.write_rows_affected(), vec![1, 0]);
        let statements = store.statements();
        let writes: Vec<_> = statements
            .iter()
            .filter(|statement| statement.sql.to_ascii_uppercase().contains("INSERT INTO"))
            .collect();
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().all(|statement| {
            let sql = statement.sql.to_ascii_uppercase();
            sql.contains("ON CONFLICT") && sql.contains("DO NOTHING")
        }));
        assert_eq!(
            statements
                .iter()
                .filter(|statement| statement.sql.to_ascii_uppercase().contains("SELECT"))
                .count(),
            2,
            "each dismissal must read back the existing timestamp"
        );
    }

    #[tokio::test]
    async fn the_same_user_id_is_read_independently_for_each_tenant() {
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, store) = connection(&[tenant_a, tenant_b], &[user_id], 1_790_000_200).await;

        let tenant_a_dismissed_at = dismiss_overview(&db, tenant_a, user_id)
            .await
            .expect("dismiss in tenant A");
        let tenant_b_dismissed_at = dismiss_overview(&db, tenant_b, user_id)
            .await
            .expect("dismiss in tenant B");

        let tenant_a_state = load_state(&db, tenant_a, user_id)
            .await
            .expect("read tenant A state");
        let tenant_b_state = load_state(&db, tenant_b, user_id)
            .await
            .expect("read tenant B state");

        assert_eq!(tenant_a_state, Some(tenant_a_dismissed_at));
        assert_eq!(tenant_b_state, Some(tenant_b_dismissed_at));
        assert_ne!(tenant_a_dismissed_at, tenant_b_dismissed_at);
        assert_eq!(store.row_count(), 2);
        assert_eq!(store.timestamp(tenant_a, user_id), Some(tenant_a_dismissed_at));
        assert_eq!(store.timestamp(tenant_b, user_id), Some(tenant_b_dismissed_at));
        let statements = store.statements();
        for statement in statements.iter().filter(|statement| {
            statement.sql.to_ascii_uppercase().contains("SELECT")
        }) {
            let bound_values = values(statement);
            assert!(bound_values.contains(&sea_orm::Value::from(user_id)));
        }
        let read_tenants: Vec<_> = statements
            .iter()
            .filter(|statement| statement.sql.to_ascii_uppercase().contains("SELECT"))
            .filter_map(|statement| {
                [tenant_a, tenant_b]
                    .into_iter()
                    .find(|tenant_id| values(statement).contains(&sea_orm::Value::from(*tenant_id)))
            })
            .collect();
        assert!(read_tenants.contains(&tenant_a));
        assert!(read_tenants.contains(&tenant_b));
    }

    #[tokio::test]
    async fn concurrent_dismissals_return_the_same_persisted_timestamp() {
        let dismissed_at = timestamp(1_790_000_300);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (db, store) = connection(&[tenant_id], &[user_id], 1_790_000_300).await;

        let (left, right) = tokio::join!(
            dismiss_overview(&db, tenant_id, user_id),
            dismiss_overview(&db, tenant_id, user_id),
        );

        assert_eq!(left.expect("first concurrent dismissal"), dismissed_at);
        assert_eq!(right.expect("second concurrent dismissal"), dismissed_at);
        assert_eq!(store.row_count(), 1);
        assert_eq!(store.timestamp(tenant_id, user_id), Some(dismissed_at));
        let write_results = store.write_rows_affected();
        assert_eq!(write_results.iter().sum::<u64>(), 1);
        assert_eq!(write_results.iter().filter(|rows| **rows == 0).count(), 1);
        let statements = store.statements();
        let writes: Vec<_> = statements
            .iter()
            .filter(|statement| statement.sql.to_ascii_uppercase().contains("INSERT INTO"))
            .collect();
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().all(|statement| {
            let sql = statement.sql.to_ascii_uppercase();
            sql.contains("ON CONFLICT") && sql.contains("DO NOTHING")
        }));
    }
}
