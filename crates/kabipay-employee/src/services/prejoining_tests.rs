//! Transaction boundary tests use a scripted PostgreSQL proxy, never a live tenant.
use super::prejoining::*;
use super::employee_service::{NewEmployee, NewLoginAccount};
use kabipay_db_entities::tenant::d0080_prejoining::prejoining_candidate as candidate;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;
use serde_json::json;
use sea_orm::DatabaseConnection;
use std::sync::{Arc, Mutex};
use std::collections::BTreeMap;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow,
    Statement,
};
use sea_orm::entity::prelude::async_trait;
fn candidate_row(tenant: Uuid, id: Uuid, status: &str) -> candidate::Model {
    candidate::Model {
        id,
        tenant_id: tenant,
        email: "candidate@example.test".into(),
        status: status.into(),
        revision: 3,
        config: json!(Config::default()),
        answers: json!({
                "firstName":"Candidate", "lastName":"Person",
                "email":"candidate@example.test"
            }),
        feedback: None,
        invitation_digest: Some(digest("private-token")),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        employee_id: if status == "JOINED" {
            Some(Uuid::new_v4())
        } else { None },
        created_by: Uuid::new_v4(),
        updated_by: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
fn candidate_proxy(row: &candidate::Model) -> ProxyRow {
    ProxyRow::new(BTreeMap::from([("id".into(), row.id.into()),
                    ("tenant_id".into(), row.tenant_id.into()),
                    ("email".into(), row.email.clone().into()),
                    ("status".into(), row.status.clone().into()),
                    ("revision".into(), row.revision.into()),
                    ("config".into(), row.config.clone().into()),
                    ("answers".into(), row.answers.clone().into()),
                    ("feedback".into(), row.feedback.clone().into()),
                    ("invitation_digest".into(),
                        row.invitation_digest.clone().into()),
                    ("expires_at".into(), row.expires_at.into()),
                    ("employee_id".into(), row.employee_id.into()),
                    ("created_by".into(), row.created_by.into()),
                    ("updated_by".into(), row.updated_by.into()),
                    ("created_at".into(), row.created_at.into()),
                    ("updated_at".into(), row.updated_at.into())]))
}
#[derive(Debug)]
struct ConversionProxy {
    fail_employee: bool,
    row: candidate::Model,
    role: Uuid,
    user: Uuid,
    events: Arc<Mutex<Vec<String>>>,
}
impl ConversionProxy {
    fn employee_row(&self) -> ProxyRow {
        let mut fields = BTreeMap::from([
            ("id".into(), self.user.into()), ("tenant_id".into(), self.row.tenant_id.into()),
            ("employee_code".into(), "EMP-TEST".into()), ("first_name".into(), "Candidate".into()),
            ("last_name".into(), "Person".into()), ("status".into(), "ACTIVE".into()),
            ("date_of_joining".into(), Utc::now().date_naive().into()), ("is_deleted".into(), false.into()),
            ("created_at".into(), Utc::now().into()), ("updated_at".into(), Utc::now().into()),
            ("notice_period_days".into(), Option::<i32>::None.into()),
            ("deleted_at".into(), Option::<DateTime<Utc>>::None.into()),
        ]);
        for name in ["user_id", "department_id", "designation_id", "cost_center_id", "location_id", "reporting_manager_id", "deleted_by"] {
            fields.insert(name.into(), Option::<Uuid>::None.into());
        }
        for name in ["date_of_birth", "probation_end_date"] {
            fields.insert(name.into(), Option::<chrono::NaiveDate>::None.into());
        }
        for name in ["gender", "blood_group", "nationality", "employment_type", "emergency_contact_name", "emergency_contact_phone", "emergency_contact_relation", "personal_phone", "current_address", "permanent_address", "uan_number", "esic_number"] {
            fields.insert(name.into(), Option::<String>::None.into());
        }
        ProxyRow::new(fields)
    }
    fn event(&self, value: String) {
        self.events.lock().unwrap().push(value);
    }
    fn role(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([("id".into(), self.role.into()),
                        ("tenant_id".into(), self.row.tenant_id.into()),
                        ("name".into(), "EMPLOYEE".into()),
                        ("description".into(), Option::<String>::None.into()),
                        ("is_system_role".into(), true.into()),
                        ("is_deleted".into(), false.into()),
                        ("deleted_at".into(), Option::<DateTime<Utc>>::None.into()),
                        ("deleted_by".into(), Option::<Uuid>::None.into()),
                        ("created_at".into(), Utc::now().into()),
                        ("updated_at".into(), Utc::now().into())]))
    }
    fn user(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([("id".into(), self.user.into()),
                        ("tenant_id".into(), self.row.tenant_id.into()),
                        ("username".into(), "candidate".into()),
                        ("email".into(),
                            Some("candidate@example.test".to_string()).into()),
                        ("password_hash".into(), "hash".into()),
                        ("must_change_password".into(), true.into()),
                        ("is_active".into(), true.into()),
                        ("mfa_enabled".into(), false.into()),
                        ("mfa_secret".into(), Option::<String>::None.into()),
                        ("last_login_at".into(),
                            Option::<DateTime<Utc>>::None.into()),
                        ("is_deleted".into(), false.into()),
                        ("deleted_at".into(), Option::<DateTime<Utc>>::None.into()),
                        ("deleted_by".into(), Option::<Uuid>::None.into()),
                        ("created_at".into(), Utc::now().into()),
                        ("updated_at".into(), Utc::now().into())]))
    }
    fn assignment(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([("user_id".into(), self.user.into()),
                        ("role_id".into(), self.role.into()),
                        ("assigned_at".into(), Utc::now().into())]))
    }
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for ConversionProxy {
    async fn begin(&self) { self.event("BEGIN".into()); }
    async fn commit(&self) { self.event("COMMIT".into()); }
    async fn rollback(&self) { self.event("ROLLBACK".into()); }
    fn start_rollback(&self) { self.event("ROLLBACK".into()); }
    async fn query(&self, statement: Statement)
        -> std::result::Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.event(sql.clone());
        if sql.contains("FROM \"prejoining_candidate\"") {
            return Ok(vec![candidate_proxy(&self.row)]);
        }
        if sql.starts_with("INSERT INTO \"employee\"") {
            if self.fail_employee {
                return Err(DbErr::Custom("simulated employee constraint failure".into()));
            }
            return Ok(vec![self.employee_row()]);
        }
        if sql.starts_with("UPDATE \"employee\"") || (sql.contains("FROM \"employee\"") && !sql.contains("\"employee_code\" =")) {
            return Ok(vec![self.employee_row()]);
        }
        if sql.starts_with("UPDATE \"prejoining_candidate\"") {
            let mut joined = self.row.clone();
            joined.status = "JOINED".into(); joined.employee_id = Some(self.user);
            joined.invitation_digest = None; joined.revision += 1;
            return Ok(vec![candidate_proxy(&joined)]);
        }
        if sql.starts_with("INSERT INTO \"prejoining_event\"") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("id".into(), Uuid::new_v4().into()), ("tenant_id".into(), self.row.tenant_id.into()),
                ("candidate_id".into(), self.row.id.into()), ("revision".into(), (self.row.revision + 1).into()),
                ("action".into(), "CONFIRM_JOINED".into()), ("snapshot".into(), json!({}).into()),
                ("actor_id".into(), Option::<Uuid>::None.into()), ("created_at".into(), Utc::now().into()),
            ]))]);
        }
        if sql.contains("FROM \"role\"") { return Ok(vec![self.role()]); }
        if sql.contains("FROM \"user_role\"") ||
                sql.starts_with("INSERT INTO \"user_role\"") {
            return Ok(vec![self.assignment()]);
        }
        if sql.starts_with("INSERT INTO \"user\"") {
            return Ok(vec![self.user()]);
        }
        if sql.contains("FROM \"user\"") && !sql.contains("\"username\" =") &&
                !sql.contains("\"email\" =") {
            return Ok(vec![self.user()]);
        }
        Ok(vec![])
    }
    async fn execute(&self, statement: Statement)
        -> std::result::Result<ProxyExecResult, DbErr> {
        self.event(statement.to_string());
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}
async fn database(row: candidate::Model)
    -> (DatabaseConnection, Uuid, Arc<Mutex<Vec<String>>>) {
    database_with_failure(row, true).await
}
async fn database_with_failure(row: candidate::Model, fail_employee: bool)
    -> (DatabaseConnection, Uuid, Arc<Mutex<Vec<String>>>) {
    let events = Arc::new(Mutex::new(vec![]));
    let role = Uuid::new_v4();
    let db =
        Database::connect_proxy(DbBackend::Postgres,
                    Arc::new(Box::new(ConversionProxy {
                                fail_employee,
                                row,
                                role,
                                user: Uuid::new_v4(),
                                events: events.clone(),
                            }))).await.unwrap();
    (db, role, events)
}

#[tokio::test]
async fn successful_conversion_creates_account_employee_and_audit_then_commits_once() {
    let row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "APPROVED");
    let (db, role, events) = database_with_failure(row.clone(), false).await;
    let joined = confirm(&db, row.tenant_id, row.id, row.revision, Uuid::new_v4(), employee(), login(role)).await.unwrap();
    assert_eq!(joined.status, "JOINED"); assert!(joined.employee_id.is_some()); assert!(joined.invitation_digest.is_none());
    let events = events.lock().unwrap();
    assert_eq!(events.iter().filter(|s| *s == "COMMIT").count(), 1, "{events:?}");
    assert!(!events.iter().any(|s| s == "ROLLBACK"));
    assert_eq!(events.iter().filter(|s| s.starts_with("INSERT INTO \"user\"")).count(), 1);
    assert_eq!(events.iter().filter(|s| s.starts_with("INSERT INTO \"employee\"")).count(), 1);
    assert!(events.iter().any(|s| s.starts_with("INSERT INTO \"prejoining_event\"") && s.contains("CONFIRM_JOINED")));
}
fn employee() -> NewEmployee {
    NewEmployee {
        employee_code: "EMP-TEST".into(),
        first_name: String::new(),
        last_name: String::new(),
        date_of_joining: Utc::now().date_naive(),
        department_id: None,
        designation_id: None,
        reporting_manager_id: None,
        employment_type: None,
        status: "ACTIVE".into(),
        user_id: None,
    }
}
fn login(role: Uuid) -> NewLoginAccount {
    NewLoginAccount {
        username: "candidate".into(),
        email: None,
        password_hash: "hash".into(),
        role_ids: vec![role],
    }
}
#[test]
fn expired_revoked_and_joined_links_are_rejected_without_erasing_hr_state() {
    let mut row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "SUBMITTED");
    assert!(check_invitation(&row,"private-token",Utc::now()).is_ok());
    assert!(check_invitation(&row,"different-token",Utc::now()).is_err());
    row.expires_at = Some(Utc::now());
    assert!(check_invitation(&row,"private-token",Utc::now()).is_err());
    assert_eq!(check_transition(&row.status,"APPROVE").unwrap(),"APPROVED");
    row.expires_at = Some(Utc::now() + Duration::hours(1));
    row.invitation_digest = None;
    assert!(check_invitation(&row,"private-token",Utc::now()).is_err());
    row.invitation_digest = Some(digest("private-token"));
    row.status = "JOINED".into();
    assert!(check_invitation(&row,"private-token",Utc::now()).is_err());
}
#[tokio::test]
async fn joined_replay_locks_tenant_candidate_and_never_creates_another_account() {
    let row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "JOINED");
    let (db, role, events) = database(row.clone()).await;
    let result =
        confirm(&db, row.tenant_id, row.id, 0, Uuid::new_v4(), employee(),
                    login(role)).await.unwrap();
    assert_eq!(result.employee_id,row.employee_id);
    let events = events.lock().unwrap();
    assert!(events.iter().any(|s|s.contains("FOR UPDATE")&&s.contains(&row.tenant_id.to_string())));
    assert!(!events.iter().any(|s|s.starts_with("INSERT")));
    assert!(!events.iter().any(|s|s=="COMMIT"));
}
#[tokio::test]
async fn stale_review_rolls_back_without_mutating_submission() {
    let row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "SUBMITTED");
    let (db, _, events) = database(row.clone()).await;
    let error =
        review(&db, row.tenant_id, row.id, 2, Uuid::new_v4(), "APPROVE",
                    None).await.unwrap_err();
    assert_eq!(error.code(),"PREJOINING_STALE_REVISION");
    let events = events.lock().unwrap();
    assert!(events.iter().any(|s|s=="ROLLBACK"));
    assert!(!events.iter().any(|s|s.starts_with("UPDATE")||s.starts_with("INSERT")));
}
#[tokio::test]
async fn employee_failure_after_account_insert_rolls_back_entire_conversion() {
    let row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "APPROVED");
    let (db, role, events) = database(row.clone()).await;
    let error =
        confirm(&db, row.tenant_id, row.id, row.revision, Uuid::new_v4(),
                    employee(), login(role)).await.unwrap_err();
    assert_eq!(error.code(),"DATABASE_ERROR");
    let events = events.lock().unwrap();
    assert!(events.iter().any(|s|s.starts_with("INSERT INTO \"user\"")),"{events:?}");
    assert!(events.iter().any(|s|s.starts_with("INSERT INTO \"employee\"")),"{events:?}");
    assert!(events.iter().any(|s|s=="ROLLBACK"));
    assert!(!events.iter().any(|s|s=="COMMIT"));
    assert!(!events.iter().any(|s|s.starts_with("UPDATE \"prejoining_candidate\"")));
}
#[tokio::test]
async fn document_metadata_queries_never_select_private_bytes() {
    let row = candidate_row(Uuid::new_v4(), Uuid::new_v4(), "SUBMITTED");
    let (db, _, events) = database(row.clone()).await;
    assert!(documents(&db,&row).await.unwrap().is_empty());
    let events = events.lock().unwrap();
    let sql = events.last().unwrap();
    assert!(sql.contains("octet_length(bytes)"));
    assert!(!sql.contains("\"prejoining_document\".\"bytes\""));
    assert!(sql.contains(&row.tenant_id.to_string())&&sql.contains(&row.id.to_string()));
}
