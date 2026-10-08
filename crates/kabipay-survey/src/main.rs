//! Privacy-preserving employee surveys GraphQL subgraph on port 4030.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph, SubgraphConfig};
use kabipay_survey::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph(
        SubgraphConfig {
            service_name: "kabipay-survey",
            default_port: 4030,
            port_env: "KABIPAY_SURVEY_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
