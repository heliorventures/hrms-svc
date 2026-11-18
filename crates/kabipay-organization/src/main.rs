#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Intentionally do not load a fleet-wide dotenv containing company secrets.
    kabipay_common::telemetry::init_tracing("kabipay-organization");
    let identities = kabipay_organization::runtime::identity_client_from_env()?;
    let url = kabipay_organization::runtime::metadata_url_from_env()?;
    let lifecycle = kabipay_common::service_lifecycle::LifecycleConfig::from_env()?;
    let port = std::env::var("KABIPAY_ORGANIZATION_PORT")
        .unwrap_or_else(|_| "4031".into())
        .parse::<u16>()?;
    let listener =
        tokio::net::TcpListener::bind(std::net::SocketAddr::from(([0, 0, 0, 0], port))).await?;
    let initialization = lifecycle.clone();
    kabipay_common::service_lifecycle::serve(listener, lifecycle, move |_| async move {
        let (store, check) =
            kabipay_organization::runtime::resilient_store(&url, &initialization).await?;
        let execution =
            if std::env::var("KABIPAY_ORGANIZATION_EXECUTION_ENABLED").as_deref() == Ok("true") {
                Some(kabipay_organization::execution::Execution::from_env(
                    store.clone(),
                    identities.clone(),
                )?)
            } else {
                None
            };
        Ok(kabipay_common::service_lifecycle::PreparedService {
            router: kabipay_organization::runtime::router_with_execution(
                store, identities, execution,
            ),
            check,
        })
    })
    .await
}
