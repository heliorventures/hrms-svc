//! Run only through scripts/test-bound-runtime.ps1, which provisions a disposable DB.
use kabipay_common::db::{
    resolve_required_tenant_db, BoundTenantDatabase, TenantDbCache, TenantDbConfig,
};
use kabipay_common::KabiPayError;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use sqlx::Connection;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable fixture from scripts/test-bound-runtime.ps1"]
async fn bound_runtime_uses_only_its_database_and_restricted_role() {
    let url = std::env::var("KABIPAY_BOUND_RUNTIME_FIXTURE_URL").expect("disposable fixture only");
    assert!(url.starts_with("postgres://bound_a_service:synthetic_runtime_a@127.0.0.1:"));
    assert!(url.ends_with("/bound_a?sslmode=disable"));
    let tenant =
        Uuid::parse_str(&std::env::var("KABIPAY_BOUND_RUNTIME_FIXTURE_TENANT").unwrap()).unwrap();
    let cache = TenantDbCache::with_binding(
        BoundTenantDatabase::from_values(
            Some(&tenant.to_string()),
            Some(&url),
            Some("tenant_fixture"),
        )
        .unwrap()
        .unwrap(),
    );
    let unused = TenantDbConfig {
        db_host: "unused-control-plane".into(),
        db_port: 1,
        db_name: "unused".into(),
        db_user: "unused".into(),
        db_password: "unused".into(),
        schema_name: "unused".into(),
    };
    let database =
        resolve_required_tenant_db(tenant, &DatabaseConnection::Disconnected, &cache, &unused)
            .await
            .unwrap();
    let marker = database
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT tenant_id FROM binding_marker".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.try_get::<Uuid>("", "tenant_id").unwrap(), tenant);
    // The synthetic registry is inside the disposable database solely for this
    // startup test; deployed control-plane credentials remain a separate role.
    cache
        .validate_binding_registration(&database)
        .await
        .unwrap();
    for mutation in [
        "UPDATE kabipay_ops.tenant_database SET db_name='bound_b'",
        "UPDATE kabipay_ops.tenant_database SET db_name='bound_a', db_type='OTHER'",
        "UPDATE kabipay_ops.tenant_database SET db_type='POSTGRES', is_active=false",
    ] {
        database
            .execute(Statement::from_string(DbBackend::Postgres, mutation))
            .await
            .unwrap();
        assert!(matches!(
            cache.validate_binding_registration(&database).await,
            Err(KabiPayError::TenantDatabaseUnavailable(_))
        ));
    }
    database
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE kabipay_ops.tenant_database SET is_active=true",
        ))
        .await
        .unwrap();
    database.execute(Statement::from_string(DbBackend::Postgres,
        "INSERT INTO kabipay_ops.tenant_database SELECT gen_random_uuid(),tenant_id,db_type,db_host,db_name,schema_name,is_active,created_at,updated_at FROM kabipay_ops.tenant_database")).await.unwrap();
    assert!(matches!(
        cache.validate_binding_registration(&database).await,
        Err(KabiPayError::TenantDatabaseUnavailable(_))
    ));
    let wrong = resolve_required_tenant_db(
        Uuid::new_v4(),
        &DatabaseConnection::Disconnected,
        &cache,
        &unused,
    )
    .await;
    assert!(matches!(wrong, Err(KabiPayError::Forbidden(_))));
    let foreign_url = url.replace("/bound_a?", "/bound_b?");
    let foreign = sqlx::postgres::PgConnection::connect(&foreign_url).await;
    assert!(
        foreign.is_err(),
        "the restricted company role must not connect to the other database"
    );
}
