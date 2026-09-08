//! Export the actual attendance GraphQL contract without connecting to databases.
use async_graphql::{EmptySubscription, Schema};
use kabipay_attendance::{MutationRoot, QueryRoot};

fn main() {
    println!("{}", Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish().sdl());
}
