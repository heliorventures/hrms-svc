use crate::{fixed_database, handle, SourceAdapter};
use async_graphql::{ObjectType, Schema, SubscriptionType};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use kabipay_common::db::{TenantDbCache, TenantDbConfig};
use sea_orm::DatabaseConnection;
use std::sync::Arc;

pub fn routes<Q, M, S, A>(adapter: A) -> Router<Arc<Schema<Q, M, S>>>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
    A: SourceAdapter + Clone + 'static,
{
    Router::new()
        .route(
            "/internal/organization/command",
            post(
                move |State(schema): State<Arc<Schema<Q, M, S>>>,
                      headers: HeaderMap,
                      body: Bytes| {
                    let adapter = adapter.clone();
                    async move {
                        let fail = || {
                            (
                                StatusCode::SERVICE_UNAVAILABLE,
                                "Organization runtime requires a fixed company binding".to_owned(),
                            )
                        };
                        let cache = schema.data::<TenantDbCache>().ok_or_else(fail)?;
                        let ops = schema.data::<DatabaseConnection>().ok_or_else(fail)?;
                        let fallback = schema.data::<TenantDbConfig>().ok_or_else(fail)?;
                        let (tenant, db) = fixed_database(cache, ops, fallback)
                            .await
                            .map_err(|_| fail())?;
                        handle(adapter, tenant, db, ops, headers, body).await
                    }
                },
            ),
        )
        .layer(DefaultBodyLimit::max(65536))
}
