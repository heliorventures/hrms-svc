// Caller drift alarm; owned transaction tests provide the commit/rollback evidence.
const SOURCE: &str = include_str!("../src/services/tax_service.rs");
#[test]
fn submit_tax_proof_line_has_transaction_owned_email_bridge() {
    let start = SOURCE.find("pub async fn submit_tax_proof_line").unwrap();
    let body = &SOURCE[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body
        .find("emit_tax_email")
        .is_some_and(|i| i < body.rfind("commit()").unwrap()));
}
#[test]
fn approve_tax_proof_line_has_transaction_owned_email_bridge() {
    let start = SOURCE.find("pub async fn approve_tax_proof_line").unwrap();
    let body = &SOURCE[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body
        .find("emit_tax_email")
        .is_some_and(|i| i < body.rfind("commit()").unwrap()));
}
#[test]
fn reject_tax_proof_line_has_transaction_owned_email_bridge() {
    let start = SOURCE.find("pub async fn reject_tax_proof_line").unwrap();
    let body = &SOURCE[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body
        .find("emit_tax_email")
        .is_some_and(|i| i < body.rfind("commit()").unwrap()));
}
