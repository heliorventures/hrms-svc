//! Loopback-only synthetic process exposing the production command/worker paths.
//! Worker execution is controlled so restart/revocation cases are deterministic.
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use sea_orm::{Database, DatabaseConnection};
use uuid::Uuid;

fn fixture_url(name: &str) -> anyhow::Result<String> {
    let url = std::env::var(name)?;
    let target = url
        .strip_prefix("postgres://")
        .and_then(|v| v.split_once('/'));
    let local_fixture = target.is_some_and(|(authority, path)| {
        let host = authority.rsplit_once('@').map(|(_, host)| host);
        host.and_then(|h| h.split_once(':'))
            .is_some_and(|(host, port)| {
                host == "127.0.0.1" && port.parse::<u16>().is_ok_and(|p| p > 0)
            })
            && path.split('?').next().is_some_and(|database| {
                database.ends_with("_fixture")
                    && database
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
    });
    anyhow::ensure!(
        local_fixture,
        "fixture requires an explicit loopback disposable database"
    );
    Ok(url)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tenant: Uuid = std::env::var("KABIPAY_FIXTURE_TENANT_ID")?.parse()?;
    let source = Database::connect(fixture_url("KABIPAY_FIXTURE_SOURCE_DATABASE_URL")?).await?;
    let ops = Database::connect(fixture_url("KABIPAY_FIXTURE_OPS_DATABASE_URL")?).await?;
    let (command_db, command_ops) = (source.clone(), ops.clone());
    let command = post(move |headers: HeaderMap, body: Bytes| {
        let (db, ops) = (command_db.clone(), command_ops.clone());
        async move {
            kabipay_organization_tenant::handle(
                kabipay_payroll::organization::PayrollAdapter,
                tenant,
                db,
                &ops,
                headers,
                body,
            )
            .await
        }
    });
    let run = post(move || {
        let (db, ops): (DatabaseConnection, DatabaseConnection) = (source.clone(), ops.clone());
        async move {
            kabipay_payroll::organization_job_worker::run_one(&db, &ops, tenant)
                .await
                .map(|worked| Json(serde_json::json!({"worked":worked})))
                .map_err(|_| {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "fixture worker unavailable",
                    )
                })
        }
    });
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/internal/organization/command", command)
        .route("/fixture/run-one", run)
        .layer(axum::extract::DefaultBodyLimit::max(65536));
    let port: u16 = std::env::var("KABIPAY_FIXTURE_PORT")?.parse()?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
