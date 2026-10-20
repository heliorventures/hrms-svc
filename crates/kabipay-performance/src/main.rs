//! kabipay-performance — review cycles, goals, KPIs, feedback, ratings.
//! Federated async-graphql subgraph on port 4021.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph_with_routes, SubgraphConfig};
use kabipay_performance::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot::default(), MutationRoot::default(), EmptySubscription);
    serve_subgraph_with_routes(
        SubgraphConfig {
            service_name: "kabipay-performance",
            default_port: 4021,
            port_env: "KABIPAY_PERFORMANCE_PORT",
            needs_db: true,
        },
        schema,
        kabipay_organization_tenant::routes(kabipay_performance::organization::PerformanceAdapter),
    )
    .await
}
