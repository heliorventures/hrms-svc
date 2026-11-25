// Caller drift alarm; owned transaction tests provide the commit/rollback evidence.
const SOURCE: &str = include_str!("../src/services/profile_change_service.rs");
#[test]
fn submit_request_has_transaction_owned_email_bridge() {
    let start = SOURCE.find("pub async fn submit_request").unwrap();
    let body = &SOURCE[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body
        .find("emit_profile_email")
        .is_some_and(|i| i < body.find("commit()").unwrap()));
}
#[test]
fn resolve_request_has_transaction_owned_email_bridge() {
    let start = SOURCE.find("pub async fn resolve_request").unwrap();
    let body = &SOURCE[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body
        .find("emit_profile_email")
        .is_some_and(|i| i < body.find("commit()").unwrap()));
}
