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
        connect_ops_db,TenantDbCache
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
async fn main()->anyhow::Result<()>{
    kabipay_common::load_dotenv();
    kabipay_common::telemetry::init_tracing("kabipay-notification");
    let ops=connect_ops_db(&ops_dsn_from_env()).await?;
    let cache=TenantDbCache::new();
    let fallback=tenant_db_config_from_env();
    let schema=Arc::new(Schema::build(QueryRoot,MutationRoot,EmptySubscription).extension(kabipay_common::entitlement_graphql::ModuleEntitlement("EMPLOYEE")).enable_federation().data(ops.clone()).data(cache.clone()).data(fallback.clone()).finish());
    let media=Arc::new(MediaState{
        ops,cache,fallback
    });
    let app=Router::new().route("/healthz",get(||async{
        "ok"
    })).route("/graphql",get(graphql_playground).post(tenant_graphql_post::<QueryRoot,MutationRoot,EmptySubscription>)).with_state(schema)
    .merge(Router::new().route("/files/announcement-video/upload",post(http_media::upload)).route("/files/announcement-video/play",get(http_media::play)).with_state(media).layer(axum::middleware::from_fn(http_media::response_headers)))
    .layer(tower_http::cors::CorsLayer::permissive());
    // Do not put signed query tokens in HTTP trace spans.
    let port=std::env::var("KABIPAY_NOTIFICATION_PORT").ok().and_then(|v|v.parse::<u16>().ok()).unwrap_or(4028);
    let listener=tokio::net::TcpListener::bind(SocketAddr::from(([0,0,0,0],port))).await?;
    axum::serve(listener,app).await?;
    Ok(())
}
