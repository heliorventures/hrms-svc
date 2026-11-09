//! Validate against the authoritative ops catalog, then write only company data.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::{
    ops::integration_connector, tenant::d0026_integrations::tenant_integration,
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult, Statement,
    TransactionTrait,
};
use uuid::Uuid;

const REFERENCE_SQL: &str =
    "INSERT INTO integration_connector_ref(id) VALUES($1) ON CONFLICT(id) DO NOTHING";
const CONNECT_SQL: &str = r#"
INSERT INTO tenant_integration
 (id,tenant_id,integration_connector_id,credentials_encrypted,config_json,is_active,connected_at,created_at,updated_at)
VALUES ($1,$2,$3,NULL,NULL,true,$4,$4,$4)
ON CONFLICT (tenant_id, integration_connector_id) DO UPDATE
 SET is_active=true,connected_at=EXCLUDED.connected_at,updated_at=EXCLUDED.updated_at
RETURNING id,tenant_id,integration_connector_id,credentials_encrypted,config_json,is_active,connected_at,created_at,updated_at
"#;

pub async fn connect(
    ops: &DatabaseConnection,
    company: &DatabaseConnection,
    tenant_id: Uuid,
    connector_id: Uuid,
) -> KabiPayResult<tenant_integration::Model> {
    let connector = integration_connector::Entity::find_by_id(connector_id)
        .one(ops)
        .await?
        .filter(|row| row.is_active)
        .ok_or_else(|| {
            KabiPayError::Validation("integration connector is unknown or inactive".into())
        })?;
    // Revalidate on every connect/reconnect, even when a local reference exists.
    // Activation is checked at admission, not as a distributed transaction.
    let transaction = company.begin().await?;
    let result: KabiPayResult<tenant_integration::Model> = async {
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                REFERENCE_SQL,
                vec![connector.id.into()],
            ))
            .await?;
        let now = chrono::Utc::now();
        let row = transaction
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                CONNECT_SQL,
                vec![
                    Uuid::new_v4().into(),
                    tenant_id.into(),
                    connector.id.into(),
                    now.into(),
                ],
            ))
            .await?
            .ok_or_else(|| {
                KabiPayError::Internal("integration connection returned no row".into())
            })?;
        Ok(tenant_integration::Model::from_query_result(&row, "")?)
    }
    .await;
    match result {
        Ok(row) => {
            transaction.commit().await?;
            Ok(row)
        }
        Err(error) => {
            transaction.rollback().await?;
            Err(error)
        }
    }
}
