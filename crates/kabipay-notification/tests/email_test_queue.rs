use kabipay_common::KabiPayError;
use kabipay_notification::email::test_queue::{load_test_status, queue_test, EmailTestState};
use sea_orm::{
    entity::prelude::async_trait, Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
    ProxyRow, Statement,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct QueueRows {
    actor_count: i64,
    tenant_count: i64,
    statements: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for QueueRows {
    async fn query(&self, sql: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(sql.to_string());
        let fields = if sql.sql.contains("tenant_email_settings") {
            BTreeMap::from([
                ("id".into(), Uuid::new_v4().into()), ("version".into(), 2_i64.into()),
                ("enabled".into(), false.into()), ("smtp_host".into(), "smtp.example.com".into()),
                ("smtp_port".into(), 465_i32.into()), ("tls_mode".into(), "implicit".into()),
                ("sender_name".into(), "Example".into()), ("sender_email".into(), "sender@example.com".into()),
                ("account_email".into(), "account@example.com".into()), ("credential_present".into(), true.into()),
                ("categories".into(), serde_json::json!({"leave":false,"approval":false,"payslip":false,"missed_punch_out":false}).into()),
            ])
        } else if sql.sql.contains("count(*)") {
            BTreeMap::from([
                ("actor_count".into(), self.actor_count.into()),
                ("tenant_count".into(), self.tenant_count.into()),
                ("email_now".into(), chrono::Utc::now().into()),
            ])
        } else if sql.sql.contains("last_error_code") {
            BTreeMap::from([
                ("id".into(), Uuid::new_v4().into()),
                ("state".into(), "FAILED".into()),
                (
                    "last_error_code".into(),
                    "private-recipient@example.com SMTP raw reply".into(),
                ),
            ])
        } else {
            return Err(DbErr::Custom("unexpected query".into()));
        };
        Ok(vec![ProxyRow::new(fields)])
    }
    async fn execute(&self, sql: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(sql.to_string());
        assert!(sql.sql.starts_with("SELECT pg_advisory_xact_lock"));
        Ok(ProxyExecResult {
            rows_affected: 1,
            last_insert_id: 0,
        })
    }
}

#[tokio::test]
async fn database_serialized_limits_stop_both_actor_and_tenant_overflow_before_insert() {
    for (actor_count, tenant_count) in [(3, 3), (0, 10)] {
        let rows = QueueRows {
            actor_count,
            tenant_count,
            statements: Arc::new(Mutex::new(vec![])),
        };
        let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(rows.clone())))
            .await
            .unwrap();
        let result = queue_test(&db, Uuid::new_v4(), Uuid::new_v4(), 2).await;
        assert!(matches!(
            result,
            Err(KabiPayError::BusinessRule {
                code: "EMAIL_TEST_RATE_LIMITED",
                ..
            })
        ));
        let statements = rows.statements.lock().unwrap();
        assert_eq!(statements.len(), 3);
        assert!(statements[0].contains("pg_advisory_xact_lock"));
        assert!(statements[1].contains("FOR UPDATE"));
        assert!(statements[2].contains("15 minutes") && statements[2].contains("1 hour"));
        assert!(!statements.iter().any(|sql| sql.starts_with("INSERT")));
    }
}

#[tokio::test]
async fn test_status_is_actor_and_tenant_bound_and_never_echoes_provider_text() {
    let tenant = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let rows = QueueRows {
        actor_count: 0,
        tenant_count: 0,
        statements: Arc::new(Mutex::new(vec![])),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(rows.clone())))
        .await
        .unwrap();
    let status = load_test_status(&db, tenant, actor, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(status.state, EmailTestState::Failed);
    assert!(status.code.is_none());
    let sql = &rows.statements.lock().unwrap()[0];
    assert!(sql.contains(&tenant.to_string()) && sql.contains(&format!("local:{actor}")));
    assert!(sql.contains("category='test'") && sql.contains("recipient_key="));
}
