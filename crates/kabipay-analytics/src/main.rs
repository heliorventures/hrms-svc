//! kabipay-analytics — saved reports, dashboards, workforce snapshots, and (HR) outbox inspection.
//! Federated async-graphql subgraph on port 4029.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

mod entities;
mod resolvers;
mod services;

#[cfg(test)]
mod authorization_tests;

use resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-analytics",
            default_port: 4029,
            port_env: "KABIPAY_ANALYTICS_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
