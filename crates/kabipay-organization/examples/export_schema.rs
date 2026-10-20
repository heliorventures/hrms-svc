fn main(){
    let schema=kabipay_organization::graphql::build_schema(kabipay_organization::store::Store::new(sea_orm::DatabaseConnection::Disconnected));
    print!("{}",schema.sdl());
}
