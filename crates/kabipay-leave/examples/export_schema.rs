//! Export the leave contract without connecting to a database.
#![allow(dead_code)]
#[path = "../src/resolvers/mod.rs"]
mod resolvers;
#[path = "../src/services/mod.rs"]
mod services;

fn main() {
    println!("{}", async_graphql::Schema::build(resolvers::QueryRoot, resolvers::MutationRoot, async_graphql::EmptySubscription).finish().sdl());
}
