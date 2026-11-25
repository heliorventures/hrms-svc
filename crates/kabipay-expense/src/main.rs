//! kabipay-expense — categories, policies, claims.
//! Federated async-graphql subgraph on port 4019.

use async_graphql::EmptySubscription;
use async_graphql::Schema;
use kabipay_common::subgraph::{serve_subgraph_with_routes, SubgraphConfig};
use kabipay_expense::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph_with_routes(
        SubgraphConfig {
            service_name: "kabipay-expense",
            default_port: 4019,
            port_env: "KABIPAY_EXPENSE_PORT",
            needs_db: true,
        },
        schema,
        kabipay_organization_tenant::email_routes::email_routes(kabipay_expense::email_alerts::ExpenseEmailAdapter),
    )
    .await
}
