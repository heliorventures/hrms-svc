use super::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn continuing_arrangement_retains_current_month_overdue_date() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    let first = f.today.with_day(1).unwrap();
    let separation = Uuid::new_v4();
    sql(&f.db, "INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'APPROVED',$4)",
        vec![separation.into(), f.tenant.into(), f.employee.into(), first.into()]).await;
    let decision = ExitLoanDecision {
        loan_id: loan,
        final_payroll_cycle_id: None,
        proposed_payroll_recovery: Decimal::ZERO,
        proposed_fnf_recovery: Decimal::ZERO,
        continuing: Some(ContinuingRepayment {
            first_payment_date: first,
            monthly_amount: Decimal::from(500),
            follow_up_user_id: f.manager,
            agreement_reference: "Approved continuing repayment agreement".into(),
        }),
    };
    sql(&f.db, "INSERT INTO loan_exit_arrangement(id,tenant_id,loan_id,employee_id,separation_id,origin_kind,origin_id,last_working_date,settlement_date,decision,decision_hash,actor_id) VALUES($1,$2,$3,$4,$5,'HR_APPROVAL',$6,$7,$7,$8,$9,$10)",
        vec![Uuid::new_v4().into(), f.tenant.into(), loan.into(), f.employee.into(), separation.into(),
            Uuid::new_v4().into(), first.into(), serde_json::to_value(&decision).unwrap().into(),
            command_hash(&decision).unwrap().into(), f.manager.into()]).await;
    let tx = f.db.begin().await.unwrap();
    let preview = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        f.today,
    )
    .await
    .unwrap();
    let items = preview["schedule"]["items"].as_array().unwrap();
    assert_eq!(items[0]["dueDate"], first.to_string());
    assert_eq!(items[0]["valueDate"], f.today.to_string());
    assert_eq!(
        items[1]["dueDate"],
        first
            .checked_add_months(Months::new(1))
            .unwrap()
            .to_string()
    );
    tx.rollback().await.unwrap();
}
