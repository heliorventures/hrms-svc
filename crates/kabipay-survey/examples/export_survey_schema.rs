//! Prints the locally compiled survey schema without starting a server or accessing data.
use async_graphql::{EmptySubscription, Schema};
use kabipay_survey::resolvers::{MutationRoot, QueryRoot};

fn main() {
    println!("{}", Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish().sdl());
}
