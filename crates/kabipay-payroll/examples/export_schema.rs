//! Export the payroll contract without accessing databases or starting a server.
#[path = "../src/resolvers/mod.rs"]
mod resolvers;
#[path = "../src/services/mod.rs"]
mod services;

fn main() {
    println!("{}", async_graphql::Schema::build(resolvers::QueryRoot, resolvers::MutationRoot, async_graphql::EmptySubscription).finish().sdl());
}
