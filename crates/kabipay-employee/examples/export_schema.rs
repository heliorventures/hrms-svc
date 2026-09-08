#[path = "../src/entities/mod.rs"] mod entities;
#[path = "../src/services/mod.rs"] mod services;
#[path = "../src/resolvers/mod.rs"] mod resolvers;
fn main() {
    let schema = async_graphql::Schema::build(resolvers::QueryRoot, resolvers::MutationRoot, async_graphql::EmptySubscription).enable_federation().finish();
    print!("{}", schema.sdl());
}
