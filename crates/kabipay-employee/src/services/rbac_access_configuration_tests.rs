use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const TENANT: Uuid = Uuid::from_u128(1);
const ROLE: Uuid = Uuid::from_u128(2);
const PERMISSION: Uuid = Uuid::from_u128(3);

#[derive(Debug)]
struct AccessProxy {
    queries: Mutex<VecDeque<Vec<ProxyRow>>>,
    events: Arc<Mutex<Vec<String>>>,
    fail: &'static str,
    admission_gate: Option<Arc<AdmissionGate>>,
}

#[derive(Debug)]
struct AdmissionGate {
    waiting_for_lock: tokio::sync::Notify,
    release_lock: tokio::sync::Notify,
    current_admin: AtomicBool,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for AccessProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.events
            .lock()
            .expect("events")
            .push(format!("QUERY {statement}"));
        if let Some(gate) = &self.admission_gate {
            if statement.sql.contains(" AS n,COALESCE") {
                return Ok(vec![admin(1, gate.current_admin.load(Ordering::SeqCst))]);
            }
        }
        Ok(self
            .queries
            .lock()
            .expect("queries")
            .pop_front()
            .expect("expected query"))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let rendered = format!("{statement}");
        self.events
            .lock()
            .expect("events")
            .push(format!("EXECUTE {rendered}"));
        if let Some(gate) = &self.admission_gate {
            if rendered.contains("pg_advisory_xact_lock") {
                gate.waiting_for_lock.notify_one();
                gate.release_lock.notified().await;
            }
        }
        if !self.fail.is_empty() && rendered.contains(self.fail) {
            return Err(DbErr::Custom("injected write failure".into()));
        }
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
    async fn begin(&self) {
        self.events.lock().expect("events").push("BEGIN".into());
    }
    async fn commit(&self) {
        self.events.lock().expect("events").push("COMMIT".into());
    }
    async fn rollback(&self) {
        self.events.lock().expect("events").push("ROLLBACK".into());
    }
}

fn admin(count: i64, own: bool) -> ProxyRow {
    ProxyRow::new(BTreeMap::from([
        ("n".into(), count.into()),
        ("own".into(), own.into()),
    ]))
}
fn role_row(tenant: Uuid) -> ProxyRow {
    let now = Utc::now();
    ProxyRow::new(BTreeMap::from([
        ("id".into(), ROLE.into()),
        ("tenant_id".into(), tenant.into()),
        ("name".into(), "HR".to_string().into()),
        ("description".into(), Option::<String>::None.into()),
        ("is_system_role".into(), true.into()),
        ("is_deleted".into(), false.into()),
        (
            "deleted_at".into(),
            Option::<chrono::DateTime<Utc>>::None.into(),
        ),
        ("deleted_by".into(), Option::<Uuid>::None.into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]))
}
fn permission_row() -> ProxyRow {
    ProxyRow::new(BTreeMap::from([
        ("id".into(), PERMISSION.into()),
        ("resource".into(), "loan".to_string().into()),
        ("action".into(), "read".to_string().into()),
        ("module_id".into(), Uuid::from_u128(5).into()),
        ("description".into(), Option::<String>::None.into()),
        ("created_at".into(), Utc::now().into()),
        ("updated_at".into(), Utc::now().into()),
    ]))
}
fn assignment() -> ProxyRow {
    ProxyRow::new(BTreeMap::from([
        ("role_id".into(), ROLE.into()),
        ("permission_id".into(), PERMISSION.into()),
        ("created_at".into(), Utc::now().into()),
    ]))
}
async fn connection(
    existing: bool,
    before: (i64, bool),
    after: (i64, bool),
    fail: &'static str,
) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    connection_with_previous_scopes(existing, before, after, fail, vec![]).await
}
async fn connection_with_previous_scopes(
    existing: bool,
    before: (i64, bool),
    after: (i64, bool),
    fail: &'static str,
    scopes: Vec<ProxyRow>,
) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let queries = vec![
        vec![admin(before.0, before.1)],
        vec![role_row(TENANT)],
        if existing { vec![assignment()] } else { vec![] },
        scopes,
        vec![permission_row()],
        vec![admin(after.0, after.1)],
    ];
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(AccessProxy {
            queries: Mutex::new(queries.into()),
            events: Arc::clone(&events),
            fail,
            admission_gate: None,
        })),
    )
    .await
    .expect("proxy");
    (db, events)
}

#[tokio::test]
async fn legacy_permissions_transport_does_not_activate_an_orphan_scope() {
    let old_scope = ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::from_u128(4).into()),
        ("tenant_id".into(), TENANT.into()),
        ("role_id".into(), ROLE.into()),
        ("resource".into(), "loan".to_string().into()),
        ("action".into(), "read".to_string().into()),
        ("scope_type".into(), "ALL".to_string().into()),
        ("created_at".into(), Utc::now().into()),
    ]));
    let (db, events) =
        connection_with_previous_scopes(false, (1, true), (1, true), "", vec![old_scope]).await;
    assert!(
        replace_access(&db, TENANT, ROLE, Some(vec![PERMISSION]), None, None)
            .await
            .is_err()
    );
    assert_rollback(&events, true);
}
fn scope() -> Vec<(String, String, String)> {
    vec![("loan".into(), "read".into(), "SELF".into())]
}
fn assert_rollback(events: &Arc<Mutex<Vec<String>>>, before_writes: bool) {
    let events = events.lock().expect("events");
    assert_eq!(events.first().map(String::as_str), Some("BEGIN"));
    assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
    assert!(!events.iter().any(|event| event == "COMMIT"));
    if before_writes {
        assert!(!events.iter().any(|event| event.contains("DELETE FROM")));
    }
}

#[tokio::test]
async fn role_from_another_tenant_is_rejected_under_the_transaction_lock() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(AccessProxy {
            queries: Mutex::new(
                vec![vec![admin(1, true)], vec![role_row(Uuid::from_u128(99))]].into(),
            ),
            events: Arc::clone(&events),
            fail: "",
            admission_gate: None,
        })),
    )
    .await
    .expect("proxy");
    let error = set_role_access_for_actor(&db, TENANT, ROLE, vec![PERMISSION], scope(), None)
        .await
        .expect_err("cross-tenant role");
    assert_eq!(error.code(), "NOT_FOUND");
    assert_rollback(&events, true);
}

async fn assert_revoked_direct_actor_denied(transport: u8) {
    let (db, events) = connection(true, (1, false), (1, false), "").await;
    let actor = Some(Uuid::from_u128(9));
    let result = match transport {
        0 => set_role_access_for_actor(&db, TENANT, ROLE, vec![PERMISSION], scope(), actor).await,
        1 => {
            super::super::set_role_permissions_for_actor(&db, TENANT, ROLE, vec![PERMISSION], actor)
                .await
        }
        _ => {
            super::super::set_role_permission_scopes_for_actor(&db, TENANT, ROLE, scope(), actor)
                .await
        }
    };
    let error = result.expect_err("revoked direct administrator must be denied before writes");
    assert_eq!(error.code(), "FORBIDDEN");
    assert_rollback(&events, true);
}

#[tokio::test]
async fn revoked_direct_actor_cannot_restore_role_access_with_old_claims() {
    assert_revoked_direct_actor_denied(0).await;
}

#[tokio::test]
async fn revoked_direct_actor_cannot_use_legacy_permission_transport() {
    assert_revoked_direct_actor_denied(1).await;
}

#[tokio::test]
async fn revoked_direct_actor_cannot_use_legacy_scope_transport() {
    assert_revoked_direct_actor_denied(2).await;
}

#[tokio::test]
async fn direct_actor_revoked_while_waiting_for_company_lock_is_denied_before_writes() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(AdmissionGate {
        waiting_for_lock: tokio::sync::Notify::new(),
        release_lock: tokio::sync::Notify::new(),
        current_admin: AtomicBool::new(true),
    });
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(AccessProxy {
            queries: Mutex::new(VecDeque::new()),
            events: Arc::clone(&events),
            fail: "",
            admission_gate: Some(Arc::clone(&gate)),
        })),
    )
    .await
    .expect("proxy");
    let admission = tokio::spawn(async move {
        set_role_access_for_actor(
            &db,
            TENANT,
            ROLE,
            vec![PERMISSION],
            scope(),
            Some(Uuid::from_u128(9)),
        )
        .await
    });
    gate.waiting_for_lock.notified().await;
    // Model revocation committing before the caller acquires the company lock.
    gate.current_admin.store(false, Ordering::SeqCst);
    gate.release_lock.notify_one();
    let error = admission
        .await
        .expect("admission task")
        .expect_err("fresh authority after lock");
    assert_eq!(error.code(), "FORBIDDEN");
    assert_rollback(&events, true);
    let events = events.lock().expect("events");
    let lock = events
        .iter()
        .position(|event| event.contains("pg_advisory_xact_lock"))
        .expect("lock");
    let read = events
        .iter()
        .position(|event| event.starts_with("QUERY"))
        .expect("fresh authority read");
    assert!(lock < read);
}

#[tokio::test]
async fn new_unscoped_grant_and_legacy_permissions_transport_are_rejected_before_writes() {
    for legacy in [false, true] {
        let (db, events) = connection(false, (1, true), (1, true), "").await;
        let error = replace_access(
            &db,
            TENANT,
            ROLE,
            Some(vec![PERMISSION]),
            if legacy { None } else { Some(vec![]) },
            Some(Uuid::from_u128(9)),
        )
        .await
        .expect_err("scope required");
        assert_eq!(error.code(), "VALIDATION_ERROR");
        assert_rollback(&events, true);
    }
}

#[tokio::test]
async fn one_transaction_writes_both_sets_and_revokes_sessions_before_commit() {
    let (db, events) = connection(false, (1, true), (1, true), "").await;
    set_role_access_for_actor(
        &db,
        TENANT,
        ROLE,
        vec![PERMISSION],
        scope(),
        Some(Uuid::from_u128(9)),
    )
    .await
    .expect("atomic save");
    let events = events.lock().expect("events");
    assert!(events
        .iter()
        .any(|event| event.contains("pg_advisory_xact_lock")));
    assert!(events.iter().any(|event| event.contains("FOR UPDATE")));
    let grants = events
        .iter()
        .position(|event| event.contains("INSERT INTO \"role_permission\""))
        .expect("grant insert");
    let scopes = events
        .iter()
        .position(|event| event.contains("INSERT INTO \"permission_scope\""))
        .expect("scope insert");
    let sessions = events
        .iter()
        .position(|event| event.contains("DELETE FROM user_session"))
        .expect("session revocation");
    let commit = events
        .iter()
        .position(|event| event == "COMMIT")
        .expect("commit");
    assert!(grants < scopes && scopes < sessions && sessions < commit);
}

#[tokio::test]
async fn scope_or_session_failure_rolls_back_the_entire_access_change() {
    for fail in [
        "INSERT INTO \"permission_scope\"",
        "DELETE FROM user_session",
    ] {
        let (db, events) = connection(false, (1, true), (1, true), fail).await;
        assert!(
            set_role_access_for_actor(&db, TENANT, ROLE, vec![PERMISSION], scope(), None)
                .await
                .is_err()
        );
        assert_rollback(&events, false);
    }
}

#[tokio::test]
async fn last_admin_and_self_lockout_guards_still_rollback_atomic_changes() {
    for (before, after) in [((1, true), (0, false)), ((2, true), (1, false))] {
        let (db, events) = connection(false, before, after, "").await;
        let error = set_role_access_for_actor(
            &db,
            TENANT,
            ROLE,
            vec![PERMISSION],
            scope(),
            Some(Uuid::from_u128(9)),
        )
        .await
        .expect_err("administration guard");
        assert_eq!(error.code(), "FORBIDDEN");
        assert_rollback(&events, false);
    }
}

#[tokio::test]
async fn legacy_inactive_grant_stays_unscoped_and_does_not_gain_all() {
    let (db, events) = connection(true, (1, true), (1, true), "").await;
    set_role_access_for_actor(&db, TENANT, ROLE, vec![PERMISSION], vec![], None)
        .await
        .expect("retain inactive extra");
    let events = events.lock().expect("events");
    assert!(!events
        .iter()
        .any(|event| event.contains("INSERT INTO \"permission_scope\"")));
    assert_eq!(events.last().map(String::as_str), Some("COMMIT"));
}

#[tokio::test]
async fn legacy_scope_transport_cannot_create_orphan_authority() {
    let (db, events) = connection(false, (1, true), (1, true), "").await;
    assert!(replace_access(&db, TENANT, ROLE, None, Some(scope()), None)
        .await
        .is_err());
    assert_rollback(&events, true);
}
