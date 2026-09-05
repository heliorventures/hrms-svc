//! kabipay-succession — competencies, talent pools, succession plans.
//! Federated async-graphql subgraph on port 4023.

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
            service_name: "kabipay-succession",
            default_port: 4023,
            port_env: "KABIPAY_SUCCESSION_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
