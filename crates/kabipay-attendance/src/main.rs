//! kabipay-attendance
//!
//! Shifts, rosters, and attendance events. Federated async-graphql subgraph
//! on port 4015 (override via `KABIPAY_ATTENDANCE_PORT`).

use async_graphql::EmptySubscription;
use async_graphql::Schema;
use kabipay_attendance::{MutationRoot, QueryRoot};
use kabipay_common::subgraph::{serve_subgraph_with_routes, SubgraphConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription);
    serve_subgraph_with_routes(
        SubgraphConfig {
            service_name: "kabipay-attendance",
            default_port: 4015,
            port_env: "KABIPAY_ATTENDANCE_PORT",
            needs_db: true,
        },
        schema,
        kabipay_organization_tenant::routes(kabipay_attendance::organization::AttendanceAdapter),
    )
    .await
}
