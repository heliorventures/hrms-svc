//! kabipay-lms — learning management: skills, courses, enrolments, certifications.
//! Federated async-graphql subgraph on port 4022.

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
            service_name: "kabipay-lms",
            default_port: 4022,
            port_env: "KABIPAY_LMS_PORT",
            needs_db: true,
        },
        schema,
    )
    .await
}
