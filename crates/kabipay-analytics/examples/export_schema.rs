#[path = "../src/entities/mod.rs"] mod entities;
#[path = "../src/resolvers/mod.rs"] mod resolvers;
#[path = "../src/services/mod.rs"] mod services;
fn main() { println!("{}",async_graphql::Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,async_graphql::EmptySubscription).finish().sdl()); }
