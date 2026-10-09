//! kabipay-payroll — salary components, structures, cycles, payslips.
//! Federated async-graphql subgraph on port 4016.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};

use kabipay_payroll::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot::default(), MutationRoot::default(), EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-payroll",
            default_port: 4016,
            port_env: "KABIPAY_PAYROLL_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
