use kabipay_common::finance_contract::HrOperationState;
use kabipay_employee::services::{finance_operation, offboarding_fnf_service, separation_service};
use kabipay_loans::LoanPermission;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

#[path = "../../kabipay-loans/tests/support/fixture.rs"]
#[allow(dead_code)]
mod fixture;

static PRIVATE_ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture with forward0125"]
async fn source_adapter_status_and_fence_reject_tampering_and_never_repeat_terminal_egress() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    use kabipay_employee::services::private_delivery::{
        self, DeliveryRequest, SignedDeliveryRequest,
    };
    use sha2::Sha256;
    let (f, _private) = private_fixture().await;
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
        HrOperationState::Cancelled,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let receipt=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT intent_hash FROM private_delivery_receipt WHERE tenant_id=$1 AND intent_id=$2 AND destination_id=$3",
        [f.tenant.into(),operation.id.into(),private_delivery::PAYROLL_DESTINATION.into()])).await.unwrap().unwrap();
    let request = DeliveryRequest {
        version: 1,
        tenant_id: f.tenant,
        intent_id: operation.id,
        destination_id: private_delivery::PAYROLL_DESTINATION,
        source_generation: 1,
        destination_version: 1,
        intent_hash: receipt.try_get("", "intent_hash").unwrap(),
        lease_generation: 0,
        request_id: Uuid::new_v4(),
        issued_at: chrono::Utc::now().timestamp(),
    };
    let signed = |request: DeliveryRequest| {
        let mut mac = Hmac::<Sha256>::new_from_slice(&[7; 32]).unwrap();
        mac.update(b"kabipay-private-delivery-request-v1\0");
        mac.update(&serde_json::to_vec(&request).unwrap());
        SignedDeliveryRequest {
            request,
            signature: URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
        }
    };
    let pending = private_delivery::execute(&f.db, &f.db, &signed(request.clone()))
        .await
        .unwrap();
    assert_eq!(pending.response.status, "PENDING");
    assert_eq!(pending.response.lease_generation, 0);
    let mut wrong = request.clone();
    wrong.intent_hash = "f".repeat(64);
    assert!(private_delivery::execute(&f.db, &f.db, &signed(wrong))
        .await
        .is_err());
    let mut wrong = request.clone();
    wrong.lease_generation = 2;
    assert!(private_delivery::execute(&f.db, &f.db, &signed(wrong))
        .await
        .is_err());
    let mut attempt = request.clone();
    attempt.lease_generation = 1;
    // Explicit source HTTPS policy rejects the configured HTTP fixture before transport.
    let result = private_delivery::execute(&f.db, &f.db, &signed(attempt.clone()))
        .await
        .unwrap();
    assert_eq!(result.response.status, "PERMANENT");
    assert_eq!(result.response.outcome_code, "finance_https_required");
    let repeated = private_delivery::execute(&f.db, &f.db, &signed(attempt))
        .await
        .unwrap();
    assert_eq!(repeated.response.lease_generation, 1);
    assert_eq!(repeated.response.status, "PERMANENT");
    let mut verifier = Hmac::<Sha256>::new_from_slice(&[7; 32]).unwrap();
    verifier.update(b"kabipay-private-delivery-response-v1\0");
    verifier.update(&serde_json::to_vec(&repeated.response).unwrap());
    verifier
        .verify_slice(&URL_SAFE_NO_PAD.decode(&repeated.signature).unwrap())
        .unwrap();
    fixture::sql(
        &f.db,
        "UPDATE private_source_cutover SET producer_generation=2 WHERE tenant_id=$1",
        vec![f.tenant.into()],
    )
    .await;
    assert!(private_delivery::execute(&f.db, &f.db, &signed(request))
        .await
        .is_err());
    assert_eq!(
        finance_operation::find(&f.db, f.tenant, operation.id, false)
            .await
            .unwrap()
            .unwrap()
            .state,
        HrOperationState::Cancelled
    );
}
struct PrivateEnvironment {
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
    files: Vec<std::path::PathBuf>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}
impl Drop for PrivateEnvironment {
    fn drop(&mut self) {
        for (name, value) in &self.prior {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        for path in &self.files {
            let _ = std::fs::remove_file(path);
        }
    }
}
async fn private_fixture() -> (fixture::Fixture, PrivateEnvironment) {
    let lock = PRIVATE_ENVIRONMENT.lock().await;
    let f = fixture::Fixture::new(100000).await;
    // The disposable fixture must include forward0125; never create substitute production DDL here.
    fixture::sql(
        &f.db,
        "INSERT INTO private_source_cutover(tenant_id) VALUES($1)",
        vec![f.tenant.into()],
    )
    .await;
    use base64::{engine::general_purpose::STANDARD, Engine};
    let secret = STANDARD.encode([7u8; 32]);
    let keys = std::env::temp_dir().join(format!("finance-private-keys-{}.json", f.tenant));
    let adapter = std::env::temp_dir().join(format!("finance-private-adapter-{}.json", f.tenant));
    std::fs::write(&keys,serde_json::to_vec(&serde_json::json!({"schema_version":1,"keys":[
        {"tenant_id":f.tenant,"purpose":"audit","key_id":"test","key_b64":secret,"active":true},
        {"tenant_id":f.tenant,"purpose":"delivery","key_id":"test","key_b64":secret,"active":true}]})).unwrap()).unwrap();
    std::fs::write(&adapter,serde_json::to_vec(&serde_json::json!({"schema_version":1,"bindings":[
        {"tenant_id":f.tenant,"source_generation":1,"key_b64":secret,"allowed_webhook_hosts":[],"webhook_keys":[]}]})).unwrap()).unwrap();
    let settings = [
        (
            "KABIPAY_PRIVATE_EVENT_KEYS_FILE",
            keys.as_os_str().to_owned(),
        ),
        (
            "KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE",
            adapter.as_os_str().to_owned(),
        ),
        (
            "KABIPAY_FINANCE_PAYROLL_URL",
            std::ffi::OsString::from("http://127.0.0.1:1"),
        ),
    ];
    let prior = settings
        .iter()
        .map(|(name, _)| (*name, std::env::var_os(name)))
        .collect();
    for (name, value) in settings {
        std::env::set_var(name, value);
    }
    (
        f,
        PrivateEnvironment {
            prior,
            files: vec![keys, adapter],
            _lock: lock,
        },
    )
}

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
    assert!(matches!(table, "private_delivery_receipt"));
    f.db.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT COUNT(*) AS amount FROM {table} WHERE tenant_id=$1 AND resource_id=$2"),
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
    let (f, _private) = private_fixture().await;
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
    let (f, _private) = private_fixture().await;
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
    assert_eq!(count(&f, "private_delivery_receipt", operation.id).await, 1);
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
    let (f, _private) = private_fixture().await;
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
    assert_eq!(count(&f, "private_delivery_receipt", operation.id).await, 0);
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
    assert_eq!(count(&f, "private_delivery_receipt", operation.id).await, 1);
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
