//! kabipay-compensation — salary bands, review cycles, bonus plans.
//! Federated async-graphql subgraph on port 4024.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

mod resolvers;
mod services;

use resolvers::{QueryRoot, MutationRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-compensation",
            default_port: 4024,
            port_env: "KABIPAY_COMPENSATION_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
