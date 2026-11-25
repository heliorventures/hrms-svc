use kabipay_common::important_email::{EmailRecipientIdentity, PrivateEmailRecipient};
use uuid::Uuid;

#[test]
fn recipient_address_not_returned_to_public_api() {
    let response = PrivateEmailRecipient::new(
        EmailRecipientIdentity {
            source_tenant_id: Uuid::new_v4(),
            source_user_id: Uuid::new_v4(),
        },
        "private.person@example.invalid".into(),
        "https://source.example.invalid".into(),
    )
    .unwrap();
    let debug = format!("{response:?}");
    assert!(!debug.contains("private.person"));
    assert!(!debug.contains("source.example"));
    assert!(PrivateEmailRecipient::new(
        response.identity,
        "x@example.invalid\r\nBcc:other@example.invalid".into(),
        "https://source.example.invalid".into()
    )
    .is_err());
    assert!(PrivateEmailRecipient::new(
        response.identity,
        "x@example.invalid".into(),
        "http://source.example.invalid".into()
    )
    .is_err());
    // Private transport DTO deliberately has no async_graphql output registration.
    let schema = async_graphql::Schema::build(
        kabipay_notification::resolvers::QueryRoot,
        kabipay_notification::resolvers::MutationRoot,
        async_graphql::EmptySubscription,
    )
    .finish()
    .sdl();
    assert!(!schema.contains("PrivateEmailRecipient"));
    assert!(!schema.contains("sourceLoginOrigin"));
}
