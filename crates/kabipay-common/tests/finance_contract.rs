use kabipay_common::finance_contract::{
    sign_message, verify_message, FinanceAction, FinanceMessage, ServiceCaller,
};
use uuid::Uuid;

#[test]
fn exit_visibility_is_exact_hr_all_or_linked_self() {
    use kabipay_common::{context::ClientClaims, finance_contract::exit_visibility_employee};
    let employee = Uuid::new_v4();
    let mut claims = ClientClaims {
        sub: Uuid::new_v4(),
        iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
        exp: 2000,
        iat: 1000,
        tenant_id: Uuid::new_v4(),
        email: String::new(),
        employee_id: Some(employee),
        must_change_password: false,
        roles: vec![],
        permissions: vec!["onboarding:self".into()],
        permission_scopes: [("onboarding:self".into(), "SELF".into())].into(),
        resource_scopes: Default::default(),
    };
    assert_eq!(exit_visibility_employee(&claims).unwrap(), Some(employee));
    for scope in ["TEAM", "DEPARTMENT", "ALL"] {
        claims
            .permission_scopes
            .insert("onboarding:self".into(), scope.into());
        assert!(exit_visibility_employee(&claims).is_err());
    }
    claims
        .permission_scopes
        .insert("onboarding:self".into(), "SELF".into());
    claims.employee_id = None;
    assert!(exit_visibility_employee(&claims).is_err());
    claims.permissions = vec!["onboarding:manage".into()];
    claims
        .permission_scopes
        .insert("onboarding:manage".into(), "ALL".into());
    assert_eq!(exit_visibility_employee(&claims).unwrap(), None);
    claims
        .permission_scopes
        .insert("onboarding:manage".into(), "TEAM".into());
    assert!(exit_visibility_employee(&claims).is_err());
}

fn message() -> FinanceMessage {
    FinanceMessage {
        version: 1,
        caller: ServiceCaller::Payroll,
        recipient: ServiceCaller::Employee,
        issued_at: 1000,
        request_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        action: FinanceAction::OperationStatus,
        client_claims: None,
        input: serde_json::json!({"operationId":Uuid::new_v4()}),
    }
}

#[test]
fn funding_exceptions_require_a_reason_and_exact_company_hr_authority() {
    use kabipay_common::{context::ClientClaims, funding_exception};
    let mut claims = ClientClaims {
        sub: Uuid::new_v4(),
        iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
        exp: 2000,
        iat: 1000,
        tenant_id: Uuid::new_v4(),
        email: String::new(),
        employee_id: None,
        must_change_password: false,
        roles: vec!["ADMIN".into()],
        permissions: vec!["loan:disburse".into()],
        permission_scopes: [("loan:disburse".into(), "ALL".into())].into(),
        resource_scopes: Default::default(),
    };
    assert!(funding_exception::validate(&claims, false, None).is_ok());
    assert!(funding_exception::validate(&claims, true, None).is_err());
    assert!(funding_exception::validate(
        &claims,
        true,
        Some("Management approved retention payment")
    )
    .is_err());
    claims.permissions.push("onboarding:manage".into());
    for scope in ["SELF", "TEAM", "DEPARTMENT"] {
        claims
            .permission_scopes
            .insert("onboarding:manage".into(), scope.into());
        assert!(funding_exception::validate(&claims, true, Some("Approved exception")).is_err());
    }
    claims
        .permission_scopes
        .insert("onboarding:manage".into(), "ALL".into());
    assert!(funding_exception::validate(&claims, true, Some("  ")).is_err());
    assert!(funding_exception::validate(&claims, true, Some(&"x".repeat(2001))).is_err());
    assert!(
        funding_exception::validate(&claims, true, Some("Management and employee agreed")).is_ok()
    );
    let mut request = message();
    request.action = FinanceAction::ReadFundingContext;
    let secret = b"test-only-finance-key-at-least-32-bytes";
    assert!(verify_message(
        &sign_message(request.clone(), secret).unwrap(),
        secret,
        ServiceCaller::Employee,
        1010
    )
    .is_err());
    request.tenant_id = claims.tenant_id;
    request.client_claims = Some(claims);
    assert!(verify_message(
        &sign_message(request, secret).unwrap(),
        secret,
        ServiceCaller::Employee,
        1010
    )
    .is_ok());
}

#[test]
fn signed_service_message_binds_tenant_body_direction_and_expiry() {
    let secret = b"test-only-finance-key-at-least-32-bytes";
    let mut signed = sign_message(message(), secret).unwrap();
    assert!(verify_message(&signed, secret, ServiceCaller::Employee, 1010).is_ok());
    assert!(verify_message(&signed, secret, ServiceCaller::Payroll, 1010).is_err());
    assert!(verify_message(&signed, secret, ServiceCaller::Employee, 1061).is_err());
    assert!(verify_message(&signed, secret, ServiceCaller::Employee, 990).is_err());
    signed.message.tenant_id = Uuid::new_v4();
    assert!(verify_message(&signed, secret, ServiceCaller::Employee, 1010).is_err());
}

#[test]
fn client_tokens_short_secrets_and_wrong_service_actions_cannot_authorize_private_calls() {
    assert!(sign_message(message(), b"short").is_err());
    let secret = b"test-only-finance-key-at-least-32-bytes";
    let mut input = message();
    input.action = FinanceAction::PrepareExitApproval;
    let signed = sign_message(input, secret).unwrap();
    assert!(verify_message(&signed, secret, ServiceCaller::Employee, 1010).is_err());
    let signed = sign_message(message(), secret).unwrap();
    assert!(verify_message(
        &signed,
        b"different-finance-key-at-least-32-bytes",
        ServiceCaller::Employee,
        1010
    )
    .is_err());
}

#[test]
fn response_authentication_binds_outcome_to_the_exact_private_request() {
    use kabipay_common::finance_contract::{signed_response, verify_response};
    let secret = b"test-only-finance-key-at-least-32-bytes";
    let request = message();
    let mut response = signed_response(
        &request,
        serde_json::json!({"state":"COMMITTED"}),
        1001,
        secret,
    )
    .unwrap();
    assert_eq!(
        verify_response(&response, &request, 1010, secret).unwrap()["state"],
        "COMMITTED"
    );
    let mut other = request.clone();
    other.request_id = Uuid::new_v4();
    assert!(verify_response(&response, &other, 1010, secret).is_err());
    assert!(verify_response(&response, &request, 1062, secret).is_err());
    response.response.result = serde_json::json!({"state":"CANCELLED"});
    assert!(verify_response(&response, &request, 1010, secret).is_err());
}
