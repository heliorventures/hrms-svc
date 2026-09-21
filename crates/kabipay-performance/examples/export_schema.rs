use async_graphql::{EmptySubscription, Schema};
use kabipay_performance::resolvers::{MutationRoot, QueryRoot};

fn main() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();
    print!("{}", schema.sdl());
}
