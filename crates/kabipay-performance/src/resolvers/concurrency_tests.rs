//! Resolver SQL-boundary tests; these do not simulate PostgreSQL lock scheduling.

use std::{collections::HashMap, sync::{Arc, Mutex}};
use async_graphql::{Context, EmptySubscription, Schema};
use chrono::Utc;
use kabipay_common::{context::ClientClaims, subgraph::TenantId};
use kabipay_db_entities::tenant::{d0018_performance::goal, d0075_performance_appraisal_lifecycle::{appraisal_template, appraisal_template_section, appraisal_question, performance_participant, performance_program}};
use rust_decimal::Decimal;
use sea_orm::{entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr, EntityTrait, Iden, Iterable, ModelTrait, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use uuid::Uuid;
use super::{MutationRoot, QueryRoot};

struct TestTenantDb(Uuid, DatabaseConnection);

pub(super) async fn tenant_db(ctx: &Context<'_>, tenant: Uuid) -> async_graphql::Result<DatabaseConnection> {
    if let Some(db) = ctx.data_opt::<TestTenantDb>() {
        assert_eq!(db.0, tenant, "resolver must retain request tenant");
        return Ok(db.1.clone());
    }
    kabipay_common::subgraph::tenant_db(ctx, tenant).await
}

fn row<M: ModelTrait>(model: M) -> ProxyRow {
    ProxyRow::new(<M::Entity as EntityTrait>::Column::iter()
        .map(|column| (column.to_string(), model.get(column))).collect())
}

#[derive(Clone, Debug)]
struct Fixture {
    tenant: Uuid,
    id: Uuid,
    participant_id: Uuid,
    employee_id: Uuid,
    cycle_id: Uuid,
    template_id: Uuid,
    events: Arc<Mutex<Vec<String>>>,
    stage_after_lock: &'static str,
    empty_definition: bool,
    fail_goal_write: bool,
    initial_goals_approved: bool,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tenant: Uuid::new_v4(), id: Uuid::new_v4(), participant_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(), cycle_id: Uuid::new_v4(), template_id: Uuid::new_v4(),
            events: Arc::default(), stage_after_lock: "GOAL_SETTING", empty_definition: false,
            fail_goal_write: false, initial_goals_approved: false,
        }
    }

    fn record(&self, event: impl Into<String>) { self.events.lock().unwrap().push(event.into()); }
    fn events(&self) -> Vec<String> { self.events.lock().unwrap().clone() }

    async fn run(&self, operation: &str) -> async_graphql::Response {
        let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone()))).await.unwrap();
        let claims = ClientClaims {
            sub: self.id, iss: "kabipay-client".into(), exp: 0, iat: 0, tenant_id: self.tenant,
            email: String::new(), employee_id: Some(self.employee_id), must_change_password: false,
            roles: vec![], permissions: vec!["performance:manage".into()],
            permission_scopes: HashMap::from([("performance:manage".into(), "ALL".into())]), resource_scopes: HashMap::new(),
        };
        Schema::build(QueryRoot, MutationRoot, EmptySubscription)
            .data(TestTenantDb(self.tenant, db)).data(TenantId(self.tenant)).data(claims)
            .finish().execute(operation
                .replace("$PARTICIPANT", &self.participant_id.to_string())
                .replace("$CYCLE", &self.cycle_id.to_string())
                .replace("$TEMPLATE", &self.template_id.to_string())).await
    }

    fn template(&self, published: bool) -> ProxyRow {
        row(appraisal_template::Model { id: self.template_id, tenant_id: self.tenant, performance_program_id: self.id, version: 1, name: "Review".into(), status: if published { "PUBLISHED" } else { "DRAFT" }.into(), published_at: None, published_by: None, created_at: Utc::now(), updated_at: Utc::now() })
    }

    fn participant(&self) -> ProxyRow {
        row(performance_participant::Model {
            id: self.participant_id, tenant_id: self.tenant, review_cycle_id: self.cycle_id, employee_id: self.employee_id,
            manager_employee_id: Some(self.id), department_id: None, designation_id: None,
            work_location_id: None, appraisal_template_id: self.template_id, status: "GOAL_SETTING".into(),
            is_excluded: false, exclusion_reason: None, response_revision: 1, self_submitted_at: None,
            manager_submitted_at: None, acknowledged_at: None, acknowledgement_comment: None,
            final_rating: None, performance_band: None, created_at: Utc::now(), updated_at: Utc::now(),
        })
    }

    fn goal(&self, approved: bool) -> ProxyRow {
        row(goal::Model { id: self.id, tenant_id: self.tenant, employee_id: self.employee_id, review_cycle_id: self.cycle_id,
            parent_goal_id: None, title: "Deliver".into(), description: None, weightage: Some(Decimal::ONE_HUNDRED),
            status: if approved { "APPROVED" } else { "PROPOSED" }.into(), visibility: Some("EMPLOYEE_MANAGER".into()), created_at: Utc::now(), updated_at: Utc::now() })
    }

    fn assert_locked_transaction(&self, table: &str, parent_id: Uuid, write: &str) {
        let events = self.events();
        let begin = events.iter().position(|e| e == "BEGIN").expect("write must start transaction");
        let lock = events.iter().position(|e| e.contains(table) && e.contains("FOR UPDATE")).expect("write must lock shared parent");
        let write = events.iter().position(|e| e.starts_with(write)).expect("expected business write");
        let commit = events.iter().position(|e| e == "COMMIT").expect("successful write must commit");
        assert!(begin < lock && lock < write && write < commit, "{events:#?}");
        assert!(events[lock].contains(&self.tenant.to_string()) && events[lock].contains(&parent_id.to_string()), "lock must scope tenant and actual parent id");
    }
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn begin(&self) { self.record("BEGIN"); }
    async fn commit(&self) { self.record("COMMIT"); }
    async fn rollback(&self) { self.record("ROLLBACK"); }
    fn start_rollback(&self) { self.record("ROLLBACK"); }

    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.record(&sql);
        if sql.contains("FROM \"performance_participant\"") { return Ok(vec![self.participant()]); }
        if sql.contains("SELECT current_stage FROM review_cycle") {
            let stage = if sql.contains("FOR UPDATE") { self.stage_after_lock } else { "GOAL_SETTING" };
            return Ok(vec![ProxyRow::new(std::collections::BTreeMap::from([("current_stage".into(), stage.into())]))]);
        }
        if sql.contains("FROM performance_program p") {
            return Ok(vec![row(performance_program::Model {
                id: self.id, tenant_id: self.tenant, name: "Annual".into(), description: None,
                cadence: "MANUAL".into(), anchor_date: Utc::now().date_naive(), status: "ACTIVE".into(),
                include_calibration: false, include_acknowledgement: false, goal_weight_required: Decimal::ONE_HUNDRED,
                rating_min: Decimal::ONE, rating_max: Decimal::new(5, 0), created_by: None, created_at: Utc::now(), updated_at: Utc::now(),
            })]);
        }
        if sql.contains("FROM \"appraisal_template_section\"") {
            return Ok(if self.empty_definition { vec![] } else { vec![row(appraisal_template_section::Model {
                id: self.id, tenant_id: self.tenant, appraisal_template_id: self.template_id, title: "General".into(), description: None, display_order: 0, created_at: Utc::now(),
            })] });
        }
        if sql.contains("FROM \"appraisal_question\"") {
            return Ok(vec![row(appraisal_question::Model { id: self.id, tenant_id: self.tenant, section_id: self.id,
                parent_question_id: None, question_type: "TEXT".into(), prompt: "Progress?".into(), is_required: true,
                answerer: "BOTH".into(), self_rating_enabled: false, manager_rating_enabled: false, display_order: 0, created_at: Utc::now() })]);
        }
        if sql.contains("FROM \"appraisal_question_option\"") { return Ok(vec![]); }
        if sql.contains("FROM \"appraisal_template\"") || sql.starts_with("UPDATE \"appraisal_template\"") {
            let published = self.events().iter().any(|e| e.starts_with("UPDATE \"appraisal_template\""));
            return Ok(vec![self.template(published)]);
        }
        if sql.starts_with("INSERT INTO \"goal\"") || sql.starts_with("UPDATE \"goal\"") {
            if self.fail_goal_write { return Err(DbErr::Custom("goal write failure".into())); }
            return Ok(vec![self.goal(sql.starts_with("UPDATE"))]);
        }
        if sql.contains("FROM \"goal\"") {
            let approved = self.initial_goals_approved || self.events().iter().any(|e| e.starts_with("UPDATE \"goal\""));
            return Ok(vec![self.goal(approved)]);
        }
        Err(DbErr::Custom(format!("unexpected query: {sql}")))
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.record(statement.to_string());
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}

const PUBLISH: &str = "mutation { publishAppraisalTemplate(appraisalTemplateId: \"$TEMPLATE\") { status } }";
const PROPOSE: &str = "mutation { proposePerformanceGoal(input: { participantId: \"$PARTICIPANT\", title: \"Deliver\", weightage: \"100\" }) { status } }";
const APPROVE: &str = "mutation { approvePerformanceGoals(participantId: \"$PARTICIPANT\") { status } }";
const ADVANCE: &str = "mutation { advancePerformanceCycle(reviewCycleId: \"$CYCLE\") }";

#[tokio::test]
async fn publish_locks_template_before_definition_read_and_status_write() {
    let fixture = Fixture::new();
    let result = fixture.run(PUBLISH).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["publishAppraisalTemplate"]["status"], "PUBLISHED");
    fixture.assert_locked_transaction("appraisal_template", fixture.template_id, "UPDATE \"appraisal_template\"");
    let events = fixture.events();
    let lock = events.iter().position(|e| e.contains("FOR UPDATE")).unwrap();
    let definition = events.iter().position(|e| e.contains("FROM \"appraisal_template_section\"")).unwrap();
    assert!(lock < definition);
}

#[tokio::test]
async fn publish_invalid_definition_rolls_back_without_status_write() {
    let mut fixture = Fixture::new(); fixture.empty_definition = true;
    let result = fixture.run(PUBLISH).await;
    assert!(result.errors.iter().any(|e| e.message.contains("requires questions")), "{:?}", result.errors);
    let events = fixture.events();
    assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
    assert!(!events.iter().any(|e| e == "COMMIT" || e.starts_with("UPDATE")));
}

#[tokio::test]
async fn proposal_holds_cycle_lock_until_goal_insert_commits() {
    let fixture = Fixture::new();
    let result = fixture.run(PROPOSE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["proposePerformanceGoal"]["status"], "PROPOSED");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "INSERT INTO \"goal\"");
}

#[tokio::test]
async fn approval_locks_cycle_before_loading_goal_weights_and_updating() {
    let fixture = Fixture::new();
    let result = fixture.run(APPROVE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["approvePerformanceGoals"][0]["status"], "APPROVED");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "UPDATE \"goal\"");
    let events = fixture.events();
    let lock = events.iter().position(|e| e.contains("FOR UPDATE")).unwrap();
    let goals = events.iter().position(|e| e.contains("FROM \"goal\"")).unwrap();
    assert!(lock < goals);
}

#[tokio::test]
async fn goal_mutations_recheck_stage_after_lock_and_do_not_write_after_advance() {
    for operation in [PROPOSE, APPROVE] {
        let mut fixture = Fixture::new(); fixture.stage_after_lock = "SELF_REVIEW";
        let result = fixture.run(operation).await;
        assert!(result.errors.iter().any(|e| e.message.contains("during goal setting")), "{:?}", result.errors);
        let events = fixture.events();
        assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
        assert!(!events.iter().any(|e| e == "COMMIT" || e.contains("FROM \"goal\"") || e.starts_with("INSERT") || e.starts_with("UPDATE")), "{events:#?}");
    }
}

#[tokio::test]
async fn goal_write_failure_rolls_back_without_commit() {
    for operation in [PROPOSE, APPROVE] {
        let mut fixture = Fixture::new(); fixture.fail_goal_write = true;
        let result = fixture.run(operation).await;
        assert!(!result.errors.is_empty());
        let events = fixture.events();
        assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
        assert!(!events.iter().any(|e| e == "COMMIT"));
    }
}

#[tokio::test]
async fn advance_uses_same_cycle_lock_through_stage_write() {
    let mut fixture = Fixture::new(); fixture.initial_goals_approved = true;
    let result = fixture.run(ADVANCE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["advancePerformanceCycle"], "SELF_REVIEW");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "UPDATE review_cycle");
}
