//! Actual draft/finalization transitions in a fully owned company database.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use chrono::Datelike;
use kabipay_common::{
    context::{ClientClaims, CLIENT_JWT_ISSUER},
    important_email::*,
};
use kabipay_payroll::services::{
    automatic_payroll::PreparedEmployeePayroll,
    payroll_draft::{self, FinalizeAcknowledgement},
    payroll_finalize,
};
use owned::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::HashMap;
fn prepared(today: chrono::NaiveDate) -> PreparedEmployeePayroll {
    serde_json::from_value(serde_json::json!({
        "input": {
            "year":chrono::Datelike::year(&today),"month":chrono::Datelike::month(&today),
            "gross_rule":"SOURCE_OVERRIDE", "fixed_gross":"10000", "earned_gross_override":"10000",
            "lwp_days":"0", "lwp_divisor":"30", "lwp_basis":"GROSS",
            "lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION", "variable_allowance_ot":"0",
            "incentive":"0", "advance_already_paid":"0", "additional_deductions":[],
            "statutory_overrides":{"PF":"0","ESI":"0","PT":"0","TDS":"0"},
            "expected_earned_components":{"BASIC":"10000"}, "expected_employer_contributions":{},
            "expected_statement":{}, "historical_lwp_included":false,"ready":true
        },
        "calculation": {
            "gross":"10000","incentive":"0","total_deductions":"0","net_earned":"10000",
            "advance_already_paid":"0","remaining_payable":"10000","lwp_amount":"0",
            "lwp_days":"0","lwp_divisor":"30","lwp_basis_amount":"10000",
            "gross_rule":"SOURCE_OVERRIDE","components":{"BASIC":"10000"},"statutory":{},
            "employer":{},"additional_deductions":[]
        },
        "tax_projection":null,"contribution_evidence":null,"requires_tax_acknowledgement":false
    }))
    .unwrap()
}

#[tokio::test]
#[ignore = "owned Node fixture only; never a live DSN"]
async fn owned_draft_is_silent_and_finalization_email_is_atomic_and_once() {
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    let metadata = fixture.metadata().await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_payroll").await;
    let entitlement = kabipay_common::entitlements::Entitlements::load(&metadata, id(1))
        .await
        .unwrap();
    let today = entitlement.business_date();
    let claims = ClientClaims {
        sub: id(5),
        iss: CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        tenant_id: id(1),
        email: String::new(),
        employee_id: Some(id(2)),
        must_change_password: false,
        roles: vec![],
        permissions: vec!["payroll:manage".into()],
        permission_scopes: HashMap::from([("payroll:manage".into(), "ALL".into())]),
        resource_scopes: HashMap::new(),
    };
    owner.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO payroll_cycle(id,tenant_id,name,year,month,status,payment_date) VALUES($1,$2,'Synthetic monthly payroll',$3,$4,'DRAFT',$5)",[id(40).into(),id(1).into(),today.year().into(),(today.month() as i32).into(),today.into()])).await.unwrap();
    let input = prepared(today).input;
    owner.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO payroll_period_input(id,tenant_id,employee_id,year,month,input,revision,ready) VALUES($1,$2,$3,$4,$5,$6,1,true)",[id(41).into(),id(1).into(),id(2).into(),today.year().into(),(today.month() as i32).into(),serde_json::to_value(input).unwrap().into()])).await.unwrap();
    let draft =
        payroll_draft::calculate_payroll_cycle(&source, id(1), &claims, &entitlement, id(40), None)
            .await
            .unwrap();
    assert!(
        draft.can_finalize,
        "synthetic review is not ready: {:?}",
        draft
            .employees
            .iter()
            .map(|employee| (&employee.outcome, &employee.reason))
            .collect::<Vec<_>>()
    );
    assert_eq!(count(&owner, "important_email_intent").await, 0);
    let rows = owner
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM payslip".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.try_get::<i64>("", "n").unwrap(), 0);
    inject_intent_failure(&owner, true).await;
    assert!(payroll_finalize::finalize_payroll_cycle(
        &source,
        id(1),
        &claims,
        &entitlement,
        id(40),
        draft.revision,
        &draft.fingerprint,
        FinalizeAcknowledgement::default()
    )
    .await
    .is_err());
    assert_eq!(count(&owner, "important_email_intent").await, 0);
    let rows=owner.query_one(Statement::from_string(DbBackend::Postgres,"SELECT (SELECT count(*) FROM payslip) AS n,(SELECT status FROM payroll_cycle LIMIT 1) AS status".to_owned())).await.unwrap().unwrap();
    assert_eq!(rows.try_get::<i64>("", "n").unwrap(), 0);
    assert_eq!(rows.try_get::<String>("", "status").unwrap(), "DRAFT");
    inject_intent_failure(&owner, false).await;
    assert_eq!(
        payroll_finalize::finalize_payroll_cycle(
            &source,
            id(1),
            &claims,
            &entitlement,
            id(40),
            draft.revision,
            &draft.fingerprint,
            FinalizeAcknowledgement::default()
        )
        .await
        .unwrap()
        .payslips,
        1
    );
    let row = owner
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id,event,event_hash FROM important_email_intent".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let event: EmailEvent = serde_json::from_value(row.try_get("", "event").unwrap()).unwrap();
    assert_eq!(event.kind, EmailEventKind::PayslipAvailable);
    assert_eq!(
        event.recipient,
        EmailRecipientRef::LocalUser { user_id: id(5) },
        "the finalizer's own payslip is an availability notice, not self-approval"
    );
    let check = EmailEligibilityRequest {
        intent_id: row.try_get("", "id").unwrap(),
        event_hash: row.try_get("", "event_hash").unwrap(),
        event,
    };
    assert_eq!(
        kabipay_payroll::email_alerts::check_email_eligibility(&source, &metadata, &check)
            .await
            .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    assert!(payroll_finalize::finalize_payroll_cycle(
        &source,
        id(1),
        &claims,
        &entitlement,
        id(40),
        draft.revision,
        &draft.fingerprint,
        FinalizeAcknowledgement::default()
    )
    .await
    .is_err());
    assert_eq!(count(&owner, "important_email_intent").await, 1);
}
