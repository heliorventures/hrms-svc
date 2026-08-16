//! kabipay-assets — asset inventory, allocations, returns.
//! Federated async-graphql subgraph on port 4025.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

mod resolvers;
mod services;

use resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-assets",
            default_port: 4025,
            port_env: "KABIPAY_ASSETS_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
