use kabipay_common::{important_email::select_local_email_reviewers, KabiPayResult};
use sea_orm::{
    entity::prelude::async_trait, Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
    ProxyRow, Statement,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug)]
struct Replies {
    rows: Vec<ProxyRow>,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Replies {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        Ok(self.rows.clone())
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("reviewer selection cannot write");
    }
}
fn candidate(user: Uuid, overflow: bool) -> ProxyRow {
    ProxyRow::new(BTreeMap::from([
        ("user_id".into(), user.into()),
        ("employee_id".into(), Uuid::new_v4().into()),
        ("scope_rank".into(), 4_i32.into()),
        ("is_reporting_manager".into(), false.into()),
        ("ancestry_overflow".into(), overflow.into()),
    ]))
}
#[tokio::test]
async fn reviewers_use_one_bounded_batch_without_addresses_or_role_names() -> KabiPayResult<()> {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let user = Uuid::new_v4();
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Replies {
            rows: vec![candidate(user, false)],
            queries: queries.clone(),
        })),
    )
    .await?;
    let selected = select_local_email_reviewers(&db, tenant, subject, "leave:approve").await?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].user_id, user);
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].contains(&tenant.to_string()));
    assert!(queries[0].contains(&subject.to_string()));
    assert!(queries[0].contains("permission_scope"));
    assert!(queries[0].contains("management_assignment"));
    assert!(!queries[0].contains("email"));
    assert!(!queries[0].contains("password"));
    assert!(!queries[0].contains("r.name"));
    assert!(!queries[0].contains("SELECT *"));
    Ok(())
}
#[tokio::test]
async fn reviewer_overflow_is_refused_instead_of_silently_dropping_people() -> KabiPayResult<()> {
    for rows in [
        vec![candidate(Uuid::new_v4(), true)],
        (0..1001)
            .map(|_| candidate(Uuid::new_v4(), false))
            .collect(),
    ] {
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(Replies {
                rows,
                queries: Arc::default(),
            })),
        )
        .await?;
        assert!(
            select_local_email_reviewers(&db, Uuid::new_v4(), Uuid::new_v4(), "leave:approve")
                .await
                .is_err()
        );
    }
    Ok(())
}
#[tokio::test]
async fn reviewer_query_rejects_untrusted_permission_fragments_before_database() {
    assert!(select_local_email_reviewers(
        &sea_orm::DatabaseConnection::Disconnected,
        Uuid::new_v4(),
        Uuid::new_v4(),
        "leave:approve;SELECT"
    )
    .await
    .is_err());
}

#[tokio::test]
async fn all_scope_reviewers_follow_account_authority_without_requiring_employment(
) -> KabiPayResult<()> {
    use kabipay_common::important_email::select_all_scope_email_users;
    let user = Uuid::new_v4();
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Replies {
            rows: vec![ProxyRow::new(BTreeMap::from([(
                "user_id".into(),
                user.into(),
            )]))],
            queries: queries.clone(),
        })),
    )
    .await?;
    assert_eq!(
        select_all_scope_email_users(&db, Uuid::new_v4(), Uuid::new_v4(), "employee:write").await?,
        vec![user]
    );
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].contains("upper(trim(ps.scope_type))='ALL'"));
    assert!(queries[0].contains("ps.role_id=r.id"));
    assert!(!queries[0].contains("JOIN employee"));
    assert!(!queries[0].contains("u.email"));
    Ok(())
}
