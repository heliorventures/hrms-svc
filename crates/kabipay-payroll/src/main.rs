//! kabipay-payroll — salary components, structures, cycles, payslips.
//! Federated async-graphql subgraph on port 4016.

use async_graphql::{EmptySubscription, Schema};
use kabipay_common::subgraph::{serve_subgraph_with_startup, SubgraphConfig};

use kabipay_payroll::resolvers::{MutationRoot, QueryRoot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let schema = Schema::build(QueryRoot::default(), MutationRoot::default(), EmptySubscription);
    serve_subgraph_with_startup(
        SubgraphConfig {
            service_name: "kabipay-payroll",
            default_port: 4016,
            port_env: "KABIPAY_PAYROLL_PORT",
            needs_db: true,
        },
        schema,
        kabipay_payroll::fnf::http::routes().merge(kabipay_organization_tenant::routes(kabipay_payroll::organization::PayrollAdapter)),
        |schema| {
            kabipay_payroll::fnf::worker::start(schema.clone());
            kabipay_payroll::organization_job_worker::start(schema);
        },
    )
    .await
}
