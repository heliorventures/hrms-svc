use super::*;

async fn request_view(f: &Fixture) -> serde_json::Value {
    let tx = f.db.begin().await.unwrap();
    let page = list_requests(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        true,
        None,
        None,
        25,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    serde_json::to_value(&page.nodes[0]).unwrap()
}

async fn intermediate_approval(f: &Fixture, request: &LoanCommandResult) -> LoanCommandResult {
    f.apply(
        f.manager,
        "ALL",
        request.version,
        "intermediate",
        LoanCommand::DecideRequest {
            request_id: request.record_id,
            step_id: f.step,
            decision: RequestDecision::Approve,
            reason: "Manager reviewed".into(),
            terms: None,
            effective_from: None,
            first_due_date: None,
            agreement_reference: None,
        },
    )
    .await
    .unwrap()
}

async fn second_step(f: &Fixture) -> Uuid {
    let step = Uuid::new_v4();
    sql(&f.db, "INSERT INTO workflow_step(id,tenant_id,workflow_id,sequence_order,step_name,approver_type,approver_permission,can_skip) VALUES($1,$2,$3,2,'Final finance approval','PERMISSION','loan:approve',FALSE)", vec![step.into(), f.tenant.into(), f.workflow.into()]).await;
    step
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn request_stage_tracks_intermediate_and_final_workflow_decisions() {
    let f = Fixture::new(100000).await;
    let final_step = second_step(&f).await;
    let request = f.request(f.user, "request").await;
    assert_eq!(request_view(&f).await["isFinalApprovalStep"], false);
    let intermediate = intermediate_approval(&f, &request).await;
    assert_eq!(intermediate.state, "UNDER_REVIEW");
    assert_eq!(intermediate.loan_id, None);
    let view = request_view(&f).await;
    assert_eq!(view["isFinalApprovalStep"], true);
    assert_eq!(view["currentStepId"], final_step.to_string());
    let mut approval = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest { step_id, .. } = &mut approval {
        *step_id = final_step;
    }
    let approved = f
        .apply(f.manager, "ALL", intermediate.version, "final", approval)
        .await
        .unwrap();
    assert_eq!(approved.state, "APPROVED");
    assert!(approved.loan_id.is_some());
    assert_eq!(request_view(&f).await["isFinalApprovalStep"], false);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn returned_reason_is_visible_only_within_request_read_scope() {
    let f = Fixture::new(100000).await;
    let request = f.request(f.user, "request").await;
    f.apply(
        f.manager,
        "ALL",
        request.version,
        "return",
        LoanCommand::DecideRequest {
            request_id: request.record_id,
            step_id: f.step,
            decision: RequestDecision::Return,
            reason: "Please attach the fee receipt.".into(),
            terms: None,
            effective_from: None,
            first_due_date: None,
            agreement_reference: None,
        },
    )
    .await
    .unwrap();
    let view = request_view(&f).await;
    assert_eq!(view["state"], "RETURNED");
    assert_eq!(view["decisionReason"], "Please attach the fee receipt.");
    assert_eq!(view["isFinalApprovalStep"], false);
    let tx = f.db.begin().await.unwrap();
    let other = list_requests(
        &tx,
        &f.actor(f.other_user, "SELF", LoanPermission::Read),
        true,
        None,
        None,
        25,
    )
    .await
    .unwrap();
    assert!(other.nodes.is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn employee_can_withdraw_after_intermediate_approval() {
    let f = Fixture::new(100000).await;
    second_step(&f).await;
    let request = f.request(f.user, "request").await;
    let intermediate = intermediate_approval(&f, &request).await;
    let withdrawn = f
        .apply(
            f.user,
            "SELF",
            intermediate.version,
            "withdraw",
            LoanCommand::WithdrawRequest {
                request_id: request.record_id,
                reason: "No longer required".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(withdrawn.state, "WITHDRAWN");
    let view = request_view(&f).await;
    assert_eq!(view["currentStepId"], serde_json::Value::Null);
    assert_eq!(view["isFinalApprovalStep"], false);
}
