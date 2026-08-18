//! kabipay-employee — federated GraphQL (default `4013`) and
//! `GET /files/employee-document?token=...` (M5, HMAC time-limited download).

use std::net::SocketAddr;
use std::sync::Arc;

use async_graphql::EmptySubscription;
use async_graphql::Schema;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use kabipay_common::db::{connect_ops_db, resolve_tenant_db, TenantDbCache, TenantDbConfig};
use kabipay_common::error::KabiPayError;
use kabipay_common::load_dotenv;
use kabipay_common::subgraph::{graphql_playground, tenant_graphql_post};
use kabipay_common::subgraph::{ops_dsn_from_env, tenant_db_config_from_env};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::DatabaseConnection;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

mod entities;
mod resolvers;
mod services;

use resolvers::{MutationRoot, QueryRoot};

use services::document_file_service;
use kabipay_common::file_download_token::verify_download_token;

#[derive(Clone, serde::Deserialize)]
struct FileQuery {
    token: String,
}

#[derive(Clone)]
struct EmployeeState {
    schema: Arc<Schema<QueryRoot, MutationRoot, EmptySubscription>>,
    file_root: std::path::PathBuf,
    ops: DatabaseConnection,
    cache: TenantDbCache,
    fallback: TenantDbConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    kabipay_common::telemetry::init_tracing("kabipay-employee");

    let port: u16 = std::env::var("KABIPAY_EMPLOYEE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4013);

    let dsn = ops_dsn_from_env();
    let ops = connect_ops_db(&dsn).await?;
    let cache = TenantDbCache::new();
    let fallback = tenant_db_config_from_env();
    let file_root = document_file_service::local_file_root();
    let _ = tokio::fs::create_dir_all(&file_root).await;

    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .enable_federation()
        .data(ops.clone())
        .data(cache.clone())
        .data(fallback.clone())
        .finish();
    let schema = Arc::new(schema);

    let st = Arc::new(EmployeeState {
        schema: schema.clone(),
        file_root,
        ops,
        cache,
        fallback,
    });

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/graphql", get(graphql_playground).post(employee_graphql))
        .route("/files/employee-document", get(employee_file_download))
        .with_state(st)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, service = "kabipay-employee", "listening (graphql + file download)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn employee_graphql(
    State(st): State<Arc<EmployeeState>>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> Result<GraphQLResponse, (StatusCode, String)> {
    tenant_graphql_post(State(st.schema.clone()), headers, req).await
}

async fn employee_file_download(
    State(st): State<Arc<EmployeeState>>,
    Query(q): Query<FileQuery>,
) -> Result<axum::response::Response, KabiPayError> {
    let claims = verify_download_token(&q.token).ok_or(KabiPayError::Unauthorised)?;

    let db = resolve_tenant_db(claims.tenant_id, &st.ops, &st.cache, &st.fallback)
        .await
        .map_err(|error: KabiPayError| error)?;

    let row = file_storage::Entity::find_by_id(claims.file_storage_id)
        .filter(file_storage::Column::TenantId.eq(claims.tenant_id))
        .one(&db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "document",
            id: "requested".into(),
        })?;

    let body = document_file_service::read_stored_file_bytes(&st.file_root, &row)
        .await?;

    let ct = claims
        .mime_type
        .as_ref()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or(row.mime_type.clone())
        .unwrap_or_else(|| "application/octet-stream".into());
    let filename = row.original_filename.as_deref().unwrap_or("file");
    let disp = format!("inline; filename=\"{filename}\"");

    let res = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ct)
        .header(header::CONTENT_DISPOSITION, disp)
        .body(Body::from(body))
        .map_err(|_| KabiPayError::Internal("failed to build file response".into()))?;
    Ok(res)
}
