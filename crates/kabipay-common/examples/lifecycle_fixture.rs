//! Synthetic loopback-only fixture for the native PostgreSQL recovery rehearsal.
use kabipay_common::{
    db::{
        availability::{connect_validated, database_check},
        TenantDbCache,
    },
    service_lifecycle::{serve, LifecycleConfig, PreparedService},
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dsn = std::env::var("KABIPAY_TEST_DATABASE_URL")?;
    let parsed = reqwest::Url::parse(&dsn)?;
    anyhow::ensure!(
        parsed.host_str() == Some("127.0.0.1") && parsed.path() == "/resilience_fixture",
        "synthetic loopback database required"
    );
    let port = std::env::var("KABIPAY_TEST_PORT")?.parse::<u16>()?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let config = LifecycleConfig::from_env()?;
    let initialize = config.clone();
    serve(listener, config, move |_| async move {
        let cache = TenantDbCache::new();
        let db = connect_validated(&dsn, &cache, &initialize).await?;
        let check = database_check(db, cache);
        let router = axum::Router::new().route(
            "/initialized",
            axum::routing::get(|| async { "initialized-once" }),
        );
        Ok(PreparedService { router, check })
    })
    .await
}
