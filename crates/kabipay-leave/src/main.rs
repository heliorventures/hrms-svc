//! kabipay-leave
//!
//! Leave types, policies, balances, accrual logs, and requests.
//! Federated async-graphql subgraph on port 4014 (override via
//! `KABIPAY_LEAVE_PORT`).

use async_graphql::EmptySubscription;
use async_graphql::Schema;
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

use kabipay_leave::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-leave",
            default_port: 4014,
            port_env: "KABIPAY_LEAVE_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
