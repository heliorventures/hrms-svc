//! Export the payroll contract without accessing databases or starting a server.
use kabipay_payroll::resolvers;

fn main() {
    println!(
        "{}",
        async_graphql::Schema::build(
            resolvers::QueryRoot::default(),
            resolvers::MutationRoot::default(),
            async_graphql::EmptySubscription
        )
        .finish()
        .sdl()
    );
}
