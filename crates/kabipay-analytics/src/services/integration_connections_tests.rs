use super::integration_connections::connect;
use sea_orm::{
    Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow,
    Statement,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct Proxy {
    catalog: bool,
    state: i32,
    log: Arc<Mutex<Vec<String>>>,
    connector: Uuid,
    tenant: Uuid,
}

#[sea_orm::entity::prelude::async_trait::async_trait]
impl ProxyDatabaseTrait for Proxy {
    async fn query(&self, sql: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.log.lock().unwrap().push(sql.to_string());
        if self.state == -1 {
            return Err(DbErr::Custom("synthetic database unavailable".into()));
        }
        if self.state == 0 {
            return Ok(vec![]);
        }
        let now = chrono::Utc::now();
        let fields = if self.catalog {
            BTreeMap::from([
                ("id".into(), self.connector.into()),
                ("name".into(), "Fixture".into()),
                ("code".into(), "FIXTURE".into()),
                ("category".into(), Option::<String>::None.into()),
                ("auth_type".into(), Option::<String>::None.into()),
                ("config_schema_json".into(), Option::<String>::None.into()),
                ("is_active".into(), (self.state == 2).into()),
                ("created_at".into(), now.into()),
                ("updated_at".into(), now.into()),
            ])
        } else {
            BTreeMap::from([
                ("id".into(), Uuid::from_u128(3).into()),
                ("tenant_id".into(), self.tenant.into()),
                ("integration_connector_id".into(), self.connector.into()),
                (
                    "credentials_encrypted".into(),
                    "synthetic-preserved-secret".into(),
                ),
                (
                    "config_json".into(),
                    serde_json::json!({"preserved":true}).into(),
                ),
                ("is_active".into(), true.into()),
                ("connected_at".into(), now.into()),
                ("created_at".into(), now.into()),
                ("updated_at".into(), now.into()),
            ])
        };
        Ok(vec![ProxyRow::new(fields)])
    }
    async fn execute(&self, sql: Statement) -> Result<ProxyExecResult, DbErr> {
        assert!(!self.catalog, "catalog connection must never write");
        self.log.lock().unwrap().push(sql.to_string());
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
    async fn begin(&self) {
        self.log.lock().unwrap().push("BEGIN".into());
    }
    async fn commit(&self) {
        self.log.lock().unwrap().push("COMMIT".into());
    }
    async fn rollback(&self) {
        self.log.lock().unwrap().push("ROLLBACK".into());
    }
}
async fn database(catalog: bool, state: i32) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(vec![]));
    let proxy = Proxy {
        catalog,
        state,
        log: log.clone(),
        connector: Uuid::from_u128(1),
        tenant: Uuid::from_u128(2),
    };
    (
        Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy)))
            .await
            .unwrap(),
        log,
    )
}

#[tokio::test]
async fn missing_inactive_and_unavailable_catalog_never_write_company_data() {
    for state in [-1, 0, 1] {
        let (ops, ops_log) = database(true, state).await;
        let (company, company_log) = database(false, 2).await;
        assert!(
            connect(&ops, &company, Uuid::from_u128(2), Uuid::from_u128(1))
                .await
                .is_err()
        );
        assert!(ops_log.lock().unwrap()[0].contains("\"kabipay_ops\".\"integration_connector\""));
        assert!(company_log.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn validated_reference_and_connection_commit_together_without_overwriting_existing_secrets() {
    let (ops, ops_log) = database(true, 2).await;
    let (company, company_log) = database(false, 2).await;
    let row = connect(&ops, &company, Uuid::from_u128(2), Uuid::from_u128(1))
        .await
        .unwrap();
    assert_eq!(
        row.credentials_encrypted.as_deref(),
        Some("synthetic-preserved-secret")
    );
    assert_eq!(row.config_json, Some(serde_json::json!({"preserved":true})));
    assert_eq!(ops_log.lock().unwrap().len(), 1);
    let log = company_log.lock().unwrap();
    assert_eq!(log.first().unwrap(), "BEGIN");
    assert!(log[1].contains("integration_connector_ref"));
    assert!(log[2].contains("ON CONFLICT (tenant_id, integration_connector_id) DO UPDATE"));
    let update = log[2]
        .split("DO UPDATE")
        .nth(1)
        .unwrap()
        .split("RETURNING")
        .next()
        .unwrap();
    assert!(!update.contains("credentials_encrypted"));
    assert!(!update.contains("config_json"));
    assert_eq!(log.last().unwrap(), "COMMIT");
}

#[tokio::test]
async fn company_failure_rolls_back_reference_and_does_not_commit() {
    let (ops, _) = database(true, 2).await;
    let (company, log) = database(false, -1).await;
    assert!(
        connect(&ops, &company, Uuid::from_u128(2), Uuid::from_u128(1))
            .await
            .is_err()
    );
    let log = log.lock().unwrap();
    assert_eq!(log.last().unwrap(), "ROLLBACK");
    assert!(!log.iter().any(|entry| entry == "COMMIT"));
}
