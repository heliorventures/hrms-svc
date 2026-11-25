use kabipay_performance::services::performance_management::effective_review_manager;
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
struct Installed {
    manager: Uuid,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Installed {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        Ok(vec![ProxyRow::new(BTreeMap::from([(
            "manager_employee_id".into(),
            self.manager.into(),
        )]))])
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("read-only review manager".into()))
    }
}
#[tokio::test]
async fn current_installed_manager_is_reused_without_loading_scores_or_answers() {
    let manager = Uuid::new_v4();
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Installed {
            manager,
            queries: queries.clone(),
        })),
    )
    .await
    .unwrap();
    assert_eq!(
        effective_review_manager(
            &db,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Some(Uuid::new_v4())
        )
        .await
        .unwrap(),
        Some(manager)
    );
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].contains("management_assignment"));
    assert!(!queries[0].contains("rating"));
    assert!(!queries[0].contains("appraisal_answer"));
    assert!(!queries[0].contains("FOR SHARE"));
}

#[tokio::test]
async fn read_only_mail_and_business_decisions_share_the_current_review_assignment_guard() {
    use kabipay_performance::services::performance_management::assert_current_review_assignment;
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Installed {
            manager: Uuid::new_v4(),
            queries: queries.clone(),
        })),
    )
    .await
    .unwrap();
    for lock in [false, true] {
        assert_current_review_assignment(
            &db,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            Uuid::new_v4(),
            lock,
        )
        .await
        .unwrap();
    }
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 2);
    for sql in queries.iter() {
        for field in [
            "organization_review_manager",
            "assignment_version",
            "graph_version",
            "performance:evaluate",
        ] {
            assert!(sql.contains(field));
        }
    }
    assert!(!queries[0].contains("FOR SHARE"));
    assert!(queries[1].contains("FOR SHARE OF s"));
}
