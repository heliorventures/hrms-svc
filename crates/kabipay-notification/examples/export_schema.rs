fn main() {
 let schema=async_graphql::Schema::build(kabipay_notification::resolvers::QueryRoot,kabipay_notification::resolvers::MutationRoot,async_graphql::EmptySubscription).enable_federation().finish();
 print!("{}",schema.sdl());
}
