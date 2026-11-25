//! Notification GraphQL and private streaming announcement media.
use std::{
    net::SocketAddr,sync::Arc
};
use async_graphql::{
    Schema,EmptySubscription
};
use axum::{
    Router,routing::{
        get,post
    }
};
use kabipay_common::{
    db::{
        TenantDbCache
    },subgraph::{
        ops_dsn_from_env,tenant_db_config_from_env,graphql_playground,tenant_graphql_post
    }
};
use kabipay_notification::{
    resolvers::{
        QueryRoot,MutationRoot
    },http_media::{
        self,MediaState
    }
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    kabipay_common::load_dotenv();
    kabipay_common::telemetry::init_tracing("kabipay-notification");
    let cache = TenantDbCache::from_env()?;
    let port = std::env::var("KABIPAY_NOTIFICATION_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(4028);
    let lifecycle = kabipay_common::service_lifecycle::LifecycleConfig::from_env()?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    let initialization = lifecycle.clone();
    let (worker_context, context_ready) = tokio::sync::oneshot::channel();
    let (worker_stop, worker_shutdown) = tokio::sync::watch::channel(false);
    let mut email_worker = tokio::spawn(async move {
        if let Ok(context) = context_ready.await {
            kabipay_notification::email::worker::run_email_worker(context, worker_shutdown).await;
        }
    });
    let service_result = kabipay_common::service_lifecycle::serve(listener, lifecycle, move |readiness| async move {
        let ops = kabipay_common::db::availability::connect_validated(
            &ops_dsn_from_env(),
            &cache,
            &initialization,
        )
        .await?;
        let check = kabipay_common::db::availability::database_check(ops.clone(), cache.clone());
        let fallback = tenant_db_config_from_env();
        let schema = Arc::new(
            Schema::build(QueryRoot, MutationRoot, EmptySubscription)
                .extension(kabipay_common::entitlement_graphql::ModuleEntitlement(
                    "EMPLOYEE",
                ))
                .enable_federation()
                .data(readiness)
                .data(ops.clone())
                .data(cache.clone())
                .data(fallback.clone())
                .finish(),
        );
        let media = Arc::new(MediaState {
            ops:ops.clone(),
            cache:cache.clone(),
            fallback:fallback.clone(),
        });
        let app = Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route(
                "/graphql",
                get(graphql_playground)
                    .post(tenant_graphql_post::<QueryRoot, MutationRoot, EmptySubscription>),
            )
            .with_state(schema)
            .merge(
                Router::new()
                    .route("/files/announcement-video/upload", post(http_media::upload))
                    .route("/files/announcement-video/play", get(http_media::play))
                    .with_state(media)
                    .layer(axum::middleware::from_fn(http_media::response_headers)),
            )
            .layer(tower_http::cors::CorsLayer::permissive());
        // Email owns no readiness probe and cannot terminate the API service.
        let _ = worker_context.send(kabipay_notification::email::worker::WorkerContext {
            ops,cache,fallback,
        });
        // Signed media query tokens remain outside HTTP trace spans.
        Ok(kabipay_common::service_lifecycle::PreparedService { router: app, check })
    })
    .await;
    let _ = worker_stop.send(true);
    if tokio::time::timeout(std::time::Duration::from_secs(5), &mut email_worker).await.is_err() {
        // Any in-flight body retains its durable Submitting fence and becomes
        // Uncertain on lease expiry; shutdown never silently resets it as unsent.
        email_worker.abort();
    }
    service_result
}
