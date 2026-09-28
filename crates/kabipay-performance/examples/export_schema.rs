use async_graphql::{EmptySubscription, Schema};
use kabipay_performance::resolvers::{MutationRoot, QueryRoot};

fn main() {
    let schema = Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        EmptySubscription,
    )
    .finish();
    print!("{}", schema.sdl());
}
