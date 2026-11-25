//! Real expiry commits, isolated from all configured application databases.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use chrono::Datelike;
use kabipay_attendance::{
    attendance_day::{resolve_window, PolicyVersion},
    sweep_expired_attendance,
};
use kabipay_common::{important_email::*, tenant_business_clock::TenantBusinessClock};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::user, d0007_employee_core::employee, d0010_time_shift_roster::attendance,
    d0087_attendance_day_boundary::attendance_day_window,
};
use owned::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;
async fn events(owner: &DatabaseConnection) -> Vec<EmailEvent> {
    owner
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT event FROM important_email_intent ORDER BY created_at,id".to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_value(r.try_get("", "event").unwrap()).unwrap())
        .collect()
}
async fn insert_day(
    owner: &DatabaseConnection,
    record: Uuid,
    employee: Uuid,
    date: chrono::NaiveDate,
    zone: &str,
    complete: bool,
) -> chrono::DateTime<chrono::Utc> {
    let policy = PolicyVersion {
        id: id(20),
        effective_work_date: "2020-01-01".parse().unwrap(),
        boundary_minutes: 300,
        timezone: zone.into(),
    };
    let window = resolve_window(&[policy], date).unwrap();
    owner.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO attendance_day_window(id,tenant_id,work_date,starts_at,ends_at,timezone,boundary_minutes,policy_version_id,created_at) VALUES($1,$2,$3,$4,$5,$6,300,$7,CURRENT_TIMESTAMP) ON CONFLICT(tenant_id,work_date) DO NOTHING",[Uuid::new_v4().into(),id(1).into(),date.into(),window.starts_at.into(),window.ends_at.into(),zone.into(),id(20).into()])).await.unwrap();
    owner.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO attendance(id,tenant_id,employee_id,work_date,check_in_at,status,check_out_at) VALUES($1,$2,$3,$4,$5,$6,$7)",[record.into(),id(1).into(),employee.into(),date.into(),(window.starts_at+chrono::Duration::hours(1)).into(),if complete {"COMPLETE"} else {"OPEN"}.into(),complete.then_some(window.ends_at-chrono::Duration::minutes(5)).into()])).await.unwrap();
    window.ends_at
}
#[tokio::test]
#[ignore = "owned Node fixture only; no live DSN"]
async fn owned_missed_punch_atomic_failure_dst_business_date_and_correction() {
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    execute(&owner, "CREATE SCHEMA tenant_fixture").await;
    entity(&owner, user::Entity).await;
    entity(&owner, employee::Entity).await;
    entity(&owner, attendance::Entity).await;
    entity(&owner, attendance_day_window::Entity).await;
    synthetic_defaults(&owner).await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_attendance").await;
    let maintenance = fixture.connect("hrms_company_maintenance").await;
    let clock = TenantBusinessClock::from_name("America/New_York").unwrap();
    let recent = clock.now_date() - chrono::Duration::days(2);
    let ended = insert_day(&owner, id(40), id(2), recent, "America/New_York", false).await;
    let year = clock.now_date().year() - 1;
    let sunday = |month| {
        let mut date = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
        while date.weekday() != chrono::Weekday::Sun {
            date = date.succ_opt().unwrap();
        }
        date
    };
    let spring = (sunday(3) + chrono::Duration::days(7)).pred_opt().unwrap();
    let fall = sunday(11).pred_opt().unwrap();
    for (record, date, hours) in [(41, spring, 23), (42, fall, 25)] {
        let end = insert_day(&owner, id(record), id(2), date, "America/New_York", false).await;
        let policy = PolicyVersion {
            id: id(20),
            effective_work_date: "2020-01-01".parse().unwrap(),
            boundary_minutes: 300,
            timezone: "America/New_York".into(),
        };
        let window = resolve_window(&[policy], date).unwrap();
        assert_eq!((end - window.starts_at).num_hours(), hours);
    }
    insert_day(&owner, id(43), id(4), recent, "America/New_York", true).await;
    inject_intent_failure(&owner, true).await;
    let failed = sweep_expired_attendance(&source, id(1), clock, 100)
        .await
        .unwrap();
    assert_eq!(failed.expired, 0);
    assert!(failed.failed > 0);
    assert!(events(&owner).await.is_empty());
    assert_eq!(count(&owner, "company_event_outbox").await, 0);
    let open = owner
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM attendance WHERE status='OPEN'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.try_get::<i64>("", "n").unwrap(), 3);
    inject_intent_failure(&owner, false).await;
    let success = sweep_expired_attendance(&source, id(1), clock, 100)
        .await
        .unwrap();
    assert_eq!(success.expired, 3);
    assert_eq!(success.failed, 0);
    let all = events(&owner).await;
    assert_eq!(all.len(), 3);
    assert!(all
        .iter()
        .all(|event| event.kind == EmailEventKind::MissedPunchOut));
    let recent_event = all
        .iter()
        .find(|event| event.resource_id == id(40))
        .unwrap()
        .clone();
    assert_eq!(recent_event.occurred_at, ended);
    assert!(all
        .iter()
        .filter(|event| event.resource_id != id(40))
        .all(|event| event.expires_at < chrono::Utc::now()));
    let check = EmailEligibilityRequest {
        intent_id: id(90),
        event_hash: recent_event.canonical_hash().unwrap(),
        event: recent_event,
    };
    assert_eq!(
        kabipay_attendance::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &check
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Eligible
    );
    execute(&owner,"UPDATE attendance SET status='COMPLETE',check_out_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP WHERE id='00000000-0000-4000-8000-000000000040'").await;
    assert_eq!(
        kabipay_attendance::email_alerts::check_email_eligibility(
            &source,
            &DatabaseConnection::Disconnected,
            &check
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    assert_eq!(
        sweep_expired_attendance(&source, id(1), clock, 100)
            .await
            .unwrap()
            .expired,
        0
    );
    assert_eq!(events(&owner).await.len(), 3);
    insert_day(&owner, id(44), id(3), recent, "America/New_York", false).await;
    assert_eq!(
        sweep_expired_attendance(&maintenance, id(1), clock, 100)
            .await
            .unwrap()
            .expired,
        1
    );
    assert_eq!(events(&owner).await.len(), 4);
    let missing=owner.query_one(Statement::from_string(DbBackend::Postgres,"SELECT count(*) AS n FROM attendance WHERE status='INCOMPLETE' AND (check_out_at IS NOT NULL OR check_out_time IS NOT NULL)".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        missing.try_get::<i64>("", "n").unwrap(),
        0,
        "expiry never manufactures a checkout"
    );
}
