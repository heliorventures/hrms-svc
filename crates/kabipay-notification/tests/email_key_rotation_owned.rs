//! Uses only the launcher-owned cluster and the actual Notification SQL login.
#[allow(dead_code, unused_imports)]
#[path = "support/email_settings_fixture.rs"]
mod fixture;
use fixture::*;
use kabipay_notification::email::{
    keyring::{Keyring, SecretString},
    rotation::{rotate_email_credential, RotationMode, RotationRequest, RotationState},
    settings::{load_email_settings, save_email_settings, SecretChange},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

fn ring(tenant: Uuid, include_old: bool) -> Keyring {
    let mut keys = serde_json::json!({"v2":"AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="});
    if include_old {
        keys["v1"] = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=".into();
    }
    let mut document = serde_json::json!({"version":1,"tenants":{}});
    document["tenants"][tenant.to_string()] = serde_json::json!({"active_key_id":"v2","keys":keys});
    Keyring::from_json(&serde_json::to_vec(&document).unwrap()).unwrap()
}
async fn snapshot(db: &DatabaseConnection) -> String {
    query(
        db,
        "SELECT md5(row_to_json(s)::text) AS fingerprint FROM tenant_email_settings s",
    )
    .await
    .try_get("", "fingerprint")
    .unwrap()
}
async fn audits(db: &DatabaseConnection) -> i64 {
    query(db, "SELECT count(*) AS n FROM company_event_outbox")
        .await
        .try_get("", "n")
        .unwrap()
}
#[tokio::test]
#[ignore = "run only through the owned local PostgreSQL fixture"]
async fn owned_rotation_preview_apply_resume_and_missing_key() {
    let fixture = fixture();
    let owner = connect(&fixture, "fixture_owner").await;
    let system: String = query(
        &owner,
        "SELECT system_identifier::text AS id FROM pg_control_system()",
    )
    .await
    .try_get("", "id")
    .unwrap();
    assert_eq!(system, fixture.system_id);
    let db = connect(&fixture, "hrms_company_notification").await;
    let tenant = fixture.tenant;
    let old = keys(tenant);
    let replacement =
        || SecretChange::Replace(SecretString::new("synthetic-rotation-only".into()).unwrap());
    let saved = save_email_settings(
        &db,
        tenant,
        fixture.actors[0],
        1,
        input(replacement(), false, false),
        Some(&old),
    )
    .await
    .unwrap();
    let keys = ring(tenant, true);
    let request = RotationRequest {
        tenant_id: tenant,
        expected_database: "company_email_fixture".into(),
        expected_schema: "tenant_fixture".into(),
        expected_version: saved.version,
        current_key_id: "v1".into(),
        new_key_id: "v2".into(),
        mode: RotationMode::Preview,
    };
    let before = snapshot(&owner).await;
    let count = audits(&owner).await;
    let preview = rotate_email_credential(&db, &request, &keys).await.unwrap();
    assert_eq!(preview.state, RotationState::Preview);
    assert_eq!(preview.changed_credentials, 0);
    assert_eq!(preview.verified_credentials, 1);
    assert!(!preview.old_key_retirement_authorized);
    assert!(snapshot(&owner).await == before); // Never print cipher/nonce even on a failure.
    assert_eq!(audits(&owner).await, count);

    let mut apply = request.clone();
    apply.mode = RotationMode::Apply;
    let missing = rotate_email_credential(&db, &apply, &ring(tenant, false))
        .await
        .unwrap_err();
    let safe = format!("{missing:?}");
    assert!(!safe.contains("synthetic-rotation-only") && !safe.contains("credential_cipher"));
    assert!(snapshot(&owner).await == before);
    for mismatch in ["database", "schema", "tenant", "version"] {
        let mut bad = apply.clone();
        match mismatch {
            "database" => bad.expected_database = "company_other".into(),
            "schema" => bad.expected_schema = "tenant_other".into(),
            "tenant" => bad.tenant_id = Uuid::new_v4(),
            _ => bad.expected_version += 1,
        }
        assert!(rotate_email_credential(&db, &bad, &keys).await.is_err());
        assert!(snapshot(&owner).await == before);
    }

    // Rewrapping must not invalidate an already claimed delivery's settings fence.
    kabipay_notification::email::test_queue::queue_test(
        &db,
        tenant,
        fixture.actors[0],
        saved.version,
    )
    .await
    .unwrap();
    let claims = kabipay_notification::email::store::claim_email_batch(&db, tenant, 1)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert!(rotate_email_credential(&db, &apply, &keys).await.is_err());
    assert!(snapshot(&owner).await == before);
    assert!(kabipay_notification::email::store::skip_email_attempt(
        &db,
        &claims[0],
        kabipay_notification::email::store::DeliveryCode::Obsolete
    )
    .await
    .unwrap());

    let done = rotate_email_credential(&db, &apply, &keys).await.unwrap();
    assert_eq!(done.state, RotationState::Rotated);
    assert_eq!(done.changed_credentials, 1);
    assert_eq!(done.settings_version, saved.version + 1);
    assert_eq!(audits(&owner).await, count + 1);
    let public = load_email_settings(&db, tenant).await.unwrap();
    assert_eq!(public.sender_email, saved.sender_email);
    assert_eq!(public.enabled, saved.enabled);
    assert!(public.credential_present);
    let after = snapshot(&owner).await;
    let resumed = rotate_email_credential(&db, &apply, &keys).await.unwrap();
    assert_eq!(resumed.state, RotationState::AlreadyCurrent);
    assert_eq!(resumed.changed_credentials, 0);
    assert_eq!(resumed.settings_version, done.settings_version);
    assert!(snapshot(&owner).await == after);
    assert_eq!(audits(&owner).await, count + 1);
    // A version conflict/new credential from an administrator is never overwritten by a resume.
    let mut edited = input(SecretChange::Preserve, false, false);
    edited.sender_name = "Concurrent administrator edit".into();
    let edited = save_email_settings(
        &db,
        tenant,
        fixture.actors[0],
        public.version,
        edited,
        Some(&keys),
    )
    .await
    .unwrap();
    let again = rotate_email_credential(&db, &apply, &keys).await.unwrap();
    assert_eq!(again.settings_version, edited.version);
    assert_eq!(
        load_email_settings(&db, tenant).await.unwrap().sender_name,
        edited.sender_name
    );
    // Auditing belongs to the same transaction: failure after UPDATE restores the old cipher/version.
    let old_again = save_email_settings(
        &db,
        tenant,
        fixture.actors[0],
        edited.version,
        input(replacement(), false, false),
        Some(&old),
    )
    .await
    .unwrap();
    owner.execute_unprepared(
        "CREATE FUNCTION tenant_fixture.fail_rotation_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'FIXTURE_AUDIT_REFUSED'; END $$; CREATE TRIGGER fail_rotation_audit BEFORE INSERT ON tenant_fixture.company_event_outbox FOR EACH ROW EXECUTE FUNCTION tenant_fixture.fail_rotation_audit()"
    ).await.unwrap();
    let before_failure = snapshot(&owner).await;
    apply.expected_version = old_again.version;
    assert!(rotate_email_credential(&db, &apply, &keys).await.is_err());
    assert!(snapshot(&owner).await == before_failure);
}
