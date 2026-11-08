use kabipay_common::finance_contract::HrOperationState;
use kabipay_employee::services::{finance_operation, offboarding_fnf_service, separation_service};
use kabipay_loans::LoanPermission;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

#[path = "../../kabipay-loans/tests/support/fixture.rs"]
#[allow(dead_code)]
mod fixture;

async fn separation(
    f: &fixture::Fixture,
) -> kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation::Model {
    separation_service::insert_separation(
        &f.db,
        f.tenant,
        f.employee,
        "RESIGNATION".into(),
        Some(f.today),
        f.today,
        None,
    )
    .await
    .unwrap()
}

async fn count(f: &fixture::Fixture, table: &str, operation: Uuid) -> i64 {
    assert!(matches!(table, "employee_finance_outbox"));
    f.db.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT COUNT(*) AS amount FROM {table} WHERE tenant_id=$1 AND operation_id=$2"),
        [f.tenant.into(), operation.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "amount")
    .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn funding_status_is_scoped_and_submission_serializes_with_disbursement() {
    use kabipay_common::{financial_lock, tenant_business_clock::TenantBusinessClock};
    use kabipay_employee::services::funding_context;
    let f = fixture::Fixture::new(100000).await;
    let clock = TenantBusinessClock::from_name("UTC").unwrap();
    let tx = f.db.begin().await.unwrap();
    financial_lock::acquire(&tx, f.tenant).await.unwrap();
    let db = f.db.clone();
    let tenant = f.tenant;
    let employee = f.employee;
    let today = f.today;
    let mut submission = tokio::spawn(async move {
        separation_service::insert_separation(
            &db,
            tenant,
            employee,
            "RESIGNATION".into(),
            Some(today),
            today,
            None,
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), &mut submission)
            .await
            .is_err()
    );
    assert!(funding_context::read(&f.db, tenant, employee, today, clock)
        .await
        .unwrap()
        .pending_separation_id
        .is_none());
    tx.commit().await.unwrap();
    let row = tokio::time::timeout(std::time::Duration::from_secs(5), submission)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        funding_context::read(&f.db, tenant, employee, today, clock)
            .await
            .unwrap()
            .pending_separation_id,
        Some(row.id)
    );
    assert!(
        funding_context::read(&f.db, Uuid::new_v4(), employee, today, clock)
            .await
            .unwrap()
            .pending_separation_id
            .is_none()
    );
    assert!(funding_context::read(&f.db, tenant, f.other, today, clock)
        .await
        .unwrap()
        .pending_separation_id
        .is_none());
    assert!(
        funding_context::read(&f.db, tenant, employee, today.pred_opt().unwrap(), clock)
            .await
            .unwrap()
            .pending_separation_id
            .is_none()
    );
    separation_service::resolve_separation(&f.db, tenant, row.id, false, f.manager, today)
        .await
        .unwrap();
    assert!(funding_context::read(&f.db, tenant, employee, today, clock)
        .await
        .unwrap()
        .pending_separation_id
        .is_none());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn hr_registration_is_scoped_idempotent_and_cancellation_is_durable() {
    let f = fixture::Fixture::new(100000).await;
    let row = separation(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let review = Uuid::new_v4();
    let operation = finance_operation::register(&f.db, f.tenant, &claims, &row, review)
        .await
        .unwrap();
    assert_eq!(
        finance_operation::register(&f.db, f.tenant, &claims, &row, review)
            .await
            .unwrap()
            .id,
        operation.id
    );
    assert!(
        finance_operation::register(&f.db, f.tenant, &claims, &row, Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(
        finance_operation::find(&f.db, Uuid::new_v4(), operation.id, false)
            .await
            .unwrap()
            .is_none()
    );
    let self_claims = f.claims(f.manager, "SELF", LoanPermission::FnfRecovery);
    assert!(
        finance_operation::register(&f.db, f.tenant, &self_claims, &row, review)
            .await
            .is_err()
    );
    assert!(separation_service::resolve_separation(
        &f.db, f.tenant, row.id, true, f.manager, f.today
    )
    .await
    .is_err());
    finance_operation::cancel(&f.db, f.tenant, operation.id)
        .await
        .unwrap();
    finance_operation::cancel(&f.db, f.tenant, operation.id)
        .await
        .unwrap();
    assert_eq!(
        finance_operation::find(&f.db, f.tenant, operation.id, false)
            .await
            .unwrap()
            .unwrap()
            .state,
        HrOperationState::Cancelled
    );
    assert_eq!(count(&f, "employee_finance_outbox", operation.id).await, 1);
    assert_eq!(
        offboarding_fnf_service::get_separation_tenant(&f.db, f.tenant, row.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "PENDING"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn terminal_hr_outcome_and_private_event_rollback_and_commit_together() {
    let f = fixture::Fixture::new(100000).await;
    let row = separation(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let operation = finance_operation::register(&f.db, f.tenant, &claims, &row, Uuid::new_v4())
        .await
        .unwrap();
    let authorization = Uuid::new_v4();
    let tx = f.db.begin().await.unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE separation SET status='APPROVED' WHERE tenant_id=$1 AND id=$2",
        [f.tenant.into(), row.id.into()],
    ))
    .await
    .unwrap();
    finance_operation::record_terminal(
        &tx,
        f.tenant,
        &operation,
        HrOperationState::Committed,
        Some(authorization),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        finance_operation::find(&f.db, f.tenant, operation.id, false)
            .await
            .unwrap()
            .unwrap()
            .state,
        HrOperationState::Registered
    );
    assert_eq!(count(&f, "employee_finance_outbox", operation.id).await, 0);
    assert_eq!(
        offboarding_fnf_service::get_separation_tenant(&f.db, f.tenant, row.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "PENDING"
    );
    let tx = f.db.begin().await.unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE separation SET status='APPROVED' WHERE tenant_id=$1 AND id=$2",
        [f.tenant.into(), row.id.into()],
    ))
    .await
    .unwrap();
    finance_operation::record_terminal(
        &tx,
        f.tenant,
        &operation,
        HrOperationState::Committed,
        Some(authorization),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        finance_operation::find(&f.db, f.tenant, operation.id, false)
            .await
            .unwrap()
            .unwrap()
            .authorization_id,
        Some(authorization)
    );
    assert_eq!(count(&f, "employee_finance_outbox", operation.id).await, 1);
    let row = offboarding_fnf_service::get_separation_tenant(&f.db, f.tenant, row.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        finance_operation::register(&f.db, f.tenant, &claims, &row, operation.review_id)
            .await
            .unwrap()
            .id,
        operation.id
    );
}

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn set(values: &[(&'static str, String)]) -> Self {
        let prior = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self(prior)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn private_delivery_retries_authenticated_confirmation_without_repeating_hr_outcome() {
    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    use kabipay_common::finance_contract::{self, SignedFinanceMessage};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    const SECRET: &str = "isolated-fixture-private-finance-key-32-bytes";
    let f = fixture::Fixture::new(100000).await;
    let row = separation(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let operation = finance_operation::register(&f.db, f.tenant, &claims, &row, Uuid::new_v4())
        .await
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    finance_operation::record_terminal(
        &tx,
        f.tenant,
        &operation,
        HrOperationState::Committed,
        Some(Uuid::new_v4()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let endpoint = |State(counter): State<Arc<AtomicUsize>>,
                    Json(envelope): Json<SignedFinanceMessage>| async move {
        let message = finance_contract::verify_message(
            &envelope,
            SECRET.as_bytes(),
            finance_contract::ServiceCaller::Payroll,
            chrono::Utc::now().timestamp(),
        )
        .unwrap();
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(Json(
            finance_contract::signed_response(
                message,
                serde_json::json!({"operationId":message.input["operationId"],"state":"CONFIRMED"}),
                chrono::Utc::now().timestamp(),
                SECRET.as_bytes(),
            )
            .unwrap(),
        ))
    };
    let app = Router::new()
        .route("/internal/finance", post(endpoint))
        .with_state(attempts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let _environment = Environment::set(&[
        ("KABIPAY_FINANCE_PAYROLL_URL", format!("http://{address}")),
        ("KABIPAY_FINANCE_SERVICE_SECRET", SECRET.into()),
    ]);
    assert_eq!(
        kabipay_employee::services::finance_outbox::deliver_due(&f.db, f.tenant)
            .await
            .unwrap(),
        0
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    fixture::sql(&f.db,"UPDATE employee_finance_outbox SET available_at=NOW() WHERE tenant_id=$1 AND operation_id=$2",vec![f.tenant.into(),operation.id.into()]).await;
    assert_eq!(
        kabipay_employee::services::finance_outbox::deliver_due(&f.db, f.tenant)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        kabipay_employee::services::finance_outbox::deliver_due(&f.db, f.tenant)
            .await
            .unwrap(),
        0
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(count(&f, "employee_finance_outbox", operation.id).await, 1);
    assert_eq!(
        finance_operation::find(&f.db, f.tenant, operation.id, false)
            .await
            .unwrap()
            .unwrap()
            .state,
        HrOperationState::Committed
    );
    server.abort();
}
