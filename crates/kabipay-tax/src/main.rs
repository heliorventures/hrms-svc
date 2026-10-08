//! kabipay-tax — tax configuration, slabs, computations, statutory filings.
//! Federated async-graphql subgraph on port 4017.

use async_graphql::EmptySubscription;
use async_graphql::Schema;
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

use kabipay_tax::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-tax",
            default_port: 4017,
            port_env: "KABIPAY_TAX_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
