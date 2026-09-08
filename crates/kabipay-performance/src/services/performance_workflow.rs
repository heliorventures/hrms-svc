use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0018_performance::goal,
    d0075_performance_appraisal_lifecycle::{
        appraisal_answer, appraisal_question, appraisal_question_option, appraisal_template,
        appraisal_template_section, continuous_feedback, performance_participant,
        performance_program,
    },
};
use rust_decimal::Decimal;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AppraisalAnswerDto, AppraisalQuestionDto, AppraisalSectionDto, AppraisalTemplateDto,
    GoalDto, PerformanceFeedbackDto, PerformanceReviewDetailDto, PerformanceReviewSummaryDto,
};
use crate::services::performance_lifecycle::{self, Cadence};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerformanceCycleSweepResult {
    pub programs_checked: usize,
    pub cycles_created: usize,
    pub participants_created: u64,
}

fn cadence(value: &str) -> Option<Cadence> {
    match value {
        "MONTHLY" => Some(Cadence::Monthly),
        "QUARTERLY" => Some(Cadence::Quarterly),
        "YEARLY" => Some(Cadence::Yearly),
        "MANUAL" => Some(Cadence::Manual),
        _ => None,
    }
}

pub async fn process_due_performance_cycles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    business_date: NaiveDate,
) -> KabiPayResult<PerformanceCycleSweepResult> {
    let programs = performance_program::Entity::find()
        .filter(performance_program::Column::TenantId.eq(tenant_id))
        .filter(performance_program::Column::Status.eq("ACTIVE"))
        .filter(performance_program::Column::AnchorDate.lte(business_date))
        .order_by_asc(performance_program::Column::Id)
        .all(db)
        .await?;
    let mut result = PerformanceCycleSweepResult::default();
    for program in programs {
        result.programs_checked += 1;
        let Some(cadence) = cadence(&program.cadence) else { continue };
        let Some(period) = performance_lifecycle::period_for_scheduled_date(cadence, business_date) else { continue };
        let template = appraisal_template::Entity::find()
            .filter(appraisal_template::Column::TenantId.eq(tenant_id))
            .filter(appraisal_template::Column::PerformanceProgramId.eq(program.id))
            .filter(appraisal_template::Column::Status.eq("PUBLISHED"))
            .order_by_desc(appraisal_template::Column::Version)
            .one(db)
            .await?;
        let Some(template) = template else {
            continue;
        };
        let cycle_id = Uuid::new_v4();
        let now = Utc::now();
        let txn = db.begin().await?;
        let created = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"INSERT INTO review_cycle
                   (id, tenant_id, name, start_date, end_date, status, review_type, created_at,
                    updated_at, performance_program_id, period_key, appraisal_template_id,
                    current_stage, launched_at)
                   VALUES ($1,$2,$3,$4,$5,'ACTIVE',$6,$7,$7,$8,$9,$10,'GOAL_SETTING',$7)
                   ON CONFLICT (performance_program_id, period_key) DO NOTHING
                   RETURNING id"#,
                vec![cycle_id.into(), tenant_id.into(), format!("{} {}", program.name, period.key).into(),
                    period.start_date.into(), period.end_date.into(), program.cadence.clone().into(), now.into(),
                    program.id.into(), period.key.into(), template.id.into()],
            ))
            .await?;
        if created.is_none() {
            txn.rollback().await?;
            continue;
        }
        let inserted = txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO performance_participant
               (id, tenant_id, review_cycle_id, employee_id, manager_employee_id, department_id,
                designation_id, work_location_id, appraisal_template_id, status, is_excluded,
                response_revision, created_at, updated_at)
               SELECT gen_random_uuid(), e.tenant_id, $1, e.id, e.reporting_manager_id, e.department_id,
                      e.designation_id, e.location_id, $2, 'GOAL_SETTING', FALSE, 1, $3, $3
               FROM employee e
               WHERE e.tenant_id = $4 AND e.is_deleted = FALSE
                 AND UPPER(TRIM(e.status)) IN ('ACTIVE','PROBATION','ON_LEAVE')
                 AND e.user_id IS NOT NULL"#,
            [cycle_id.into(), template.id.into(), now.into(), tenant_id.into()],
        )).await?;
        if inserted.rows_affected() == 0 {
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO performance_admin_exception (id, tenant_id, performance_program_id, review_cycle_id, exception_code, details, created_at) VALUES ($1,$2,$3,$4,'NO_ELIGIBLE_PARTICIPANTS','No active employee-linked accounts were eligible when the automated cycle was created',$5)",
                [Uuid::new_v4().into(), tenant_id.into(), program.id.into(), cycle_id.into(), now.into()],
            )).await?;
        }
        txn.commit().await?;
        result.cycles_created += 1;
        result.participants_created += inserted.rows_affected();
    }
    Ok(result)
}

pub async fn list_programs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<performance_program::Model>> {
    performance_program::Entity::find()
        .filter(performance_program::Column::TenantId.eq(tenant_id))
        .order_by_asc(performance_program::Column::Name)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_templates(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    program_id: Uuid,
) -> KabiPayResult<Vec<appraisal_template::Model>> {
    appraisal_template::Entity::find()
        .filter(appraisal_template::Column::TenantId.eq(tenant_id))
        .filter(appraisal_template::Column::PerformanceProgramId.eq(program_id))
        .order_by_desc(appraisal_template::Column::Version)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn load_template(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    template_id: Uuid,
) -> KabiPayResult<AppraisalTemplateDto> {
    let template = appraisal_template::Entity::find_by_id(template_id)
        .filter(appraisal_template::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "appraisal template",
            id: template_id.to_string(),
        })?;
    let sections = appraisal_template_section::Entity::find()
        .filter(appraisal_template_section::Column::TenantId.eq(tenant_id))
        .filter(appraisal_template_section::Column::AppraisalTemplateId.eq(template_id))
        .order_by_asc(appraisal_template_section::Column::DisplayOrder)
        .all(db)
        .await?;
    let section_ids: Vec<Uuid> = sections.iter().map(|section| section.id).collect();
    let questions = if section_ids.is_empty() {
        Vec::new()
    } else {
        appraisal_question::Entity::find()
            .filter(appraisal_question::Column::TenantId.eq(tenant_id))
            .filter(appraisal_question::Column::SectionId.is_in(section_ids))
            .order_by_asc(appraisal_question::Column::DisplayOrder)
            .all(db)
            .await?
    };
    let question_ids: Vec<Uuid> = questions.iter().map(|question| question.id).collect();
    let options = if question_ids.is_empty() {
        Vec::new()
    } else {
        appraisal_question_option::Entity::find()
            .filter(appraisal_question_option::Column::TenantId.eq(tenant_id))
            .filter(appraisal_question_option::Column::QuestionId.is_in(question_ids))
            .order_by_asc(appraisal_question_option::Column::DisplayOrder)
            .all(db)
            .await?
    };
    let sections = sections
        .into_iter()
        .map(|section| {
            let section_questions = questions
                .iter()
                .filter(|question| question.section_id == section.id)
                .cloned()
                .map(|question| {
                    let question_options = options
                        .iter()
                        .filter(|option| option.question_id == question.id)
                        .cloned()
                        .collect();
                    AppraisalQuestionDto::from_model(question, question_options)
                })
                .collect();
            AppraisalSectionDto::from_model(section, section_questions)
        })
        .collect();
    Ok(AppraisalTemplateDto::from_model(template, sections))
}

#[derive(Debug, FromQueryResult)]
struct ReviewSummaryRow {
    id: Uuid,
    review_cycle_id: Uuid,
    employee_id: Uuid,
    employee_name: String,
    manager_employee_id: Option<Uuid>,
    manager_name: Option<String>,
    appraisal_template_id: Uuid,
    cycle_name: String,
    cycle_start_date: NaiveDate,
    cycle_end_date: NaiveDate,
    cycle_stage: String,
    status: String,
    self_submitted_at: Option<DateTime<Utc>>,
    manager_submitted_at: Option<DateTime<Utc>>,
    acknowledged_at: Option<DateTime<Utc>>,
    final_rating: Option<Decimal>,
    performance_band: Option<String>,
}

impl From<ReviewSummaryRow> for PerformanceReviewSummaryDto {
    fn from(row: ReviewSummaryRow) -> Self {
        Self {
            id: row.id.to_string().into(),
            review_cycle_id: row.review_cycle_id.to_string().into(),
            employee_id: row.employee_id.to_string().into(),
            employee_name: row.employee_name,
            manager_employee_id: row.manager_employee_id.map(|id| id.to_string().into()),
            manager_name: row.manager_name,
            appraisal_template_id: row.appraisal_template_id.to_string().into(),
            cycle_name: row.cycle_name,
            cycle_start_date: row.cycle_start_date,
            cycle_end_date: row.cycle_end_date,
            cycle_stage: row.cycle_stage,
            status: row.status,
            self_submitted_at: row.self_submitted_at,
            manager_submitted_at: row.manager_submitted_at,
            acknowledged_at: row.acknowledged_at,
            final_rating: row.final_rating.map(|value| value.to_string()),
            performance_band: row.performance_band,
        }
    }
}

const REVIEW_SUMMARY_SQL: &str = r#"
SELECT p.id, p.review_cycle_id, p.employee_id,
       BTRIM(CONCAT(e.first_name, ' ', e.last_name)) AS employee_name,
       p.manager_employee_id,
       CASE WHEN m.id IS NULL THEN NULL ELSE BTRIM(CONCAT(m.first_name, ' ', m.last_name)) END AS manager_name,
       p.appraisal_template_id, c.name AS cycle_name,
       c.start_date AS cycle_start_date, c.end_date AS cycle_end_date,
       c.current_stage AS cycle_stage, p.status, p.self_submitted_at,
       p.manager_submitted_at, p.acknowledged_at, p.final_rating, p.performance_band
FROM performance_participant p
JOIN review_cycle c ON c.id = p.review_cycle_id AND c.tenant_id = p.tenant_id
JOIN employee e ON e.id = p.employee_id AND e.tenant_id = p.tenant_id
LEFT JOIN employee m ON m.id = p.manager_employee_id AND m.tenant_id = p.tenant_id
WHERE p.tenant_id = $1 AND p.is_excluded = FALSE
"#;

pub async fn list_reviews_for_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Vec<PerformanceReviewSummaryDto>> {
    review_summaries(db, format!("{REVIEW_SUMMARY_SQL} AND p.employee_id = $2 ORDER BY c.start_date DESC"), vec![tenant_id.into(), employee_id.into()]).await
}

pub async fn list_reviews_for_manager(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    manager_employee_id: Uuid,
) -> KabiPayResult<Vec<PerformanceReviewSummaryDto>> {
    review_summaries(db, format!("{REVIEW_SUMMARY_SQL} AND p.manager_employee_id = $2 ORDER BY c.start_date DESC, employee_name"), vec![tenant_id.into(), manager_employee_id.into()]).await
}

pub async fn list_all_reviews(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<PerformanceReviewSummaryDto>> {
    review_summaries(db, format!("{REVIEW_SUMMARY_SQL} ORDER BY c.start_date DESC, employee_name"), vec![tenant_id.into()]).await
}

async fn review_summaries(
    db: &DatabaseConnection,
    sql: String,
    values: Vec<sea_orm::Value>,
) -> KabiPayResult<Vec<PerformanceReviewSummaryDto>> {
    ReviewSummaryRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        values,
    ))
    .all(db)
    .await
    .map(|rows| rows.into_iter().map(Into::into).collect())
    .map_err(KabiPayError::from)
}

pub async fn load_participant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> KabiPayResult<performance_participant::Model> {
    performance_participant::Entity::find_by_id(participant_id)
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance review",
            id: participant_id.to_string(),
        })
}

pub async fn load_review_detail(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    participant: performance_participant::Model,
) -> KabiPayResult<PerformanceReviewDetailDto> {
    let mut summaries = review_summaries(
        db,
        format!("{REVIEW_SUMMARY_SQL} AND p.id = $2"),
        vec![tenant_id.into(), participant.id.into()],
    )
    .await?;
    let review = summaries.pop().ok_or_else(|| KabiPayError::NotFound {
        entity: "performance review",
        id: participant.id.to_string(),
    })?;
    let goals = goal::Entity::find()
        .filter(goal::Column::TenantId.eq(tenant_id))
        .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
        .filter(goal::Column::EmployeeId.eq(participant.employee_id))
        .order_by_asc(goal::Column::CreatedAt)
        .all(db)
        .await?
        .into_iter()
        .map(GoalDto::from)
        .collect();
    let feedback = continuous_feedback::Entity::find()
        .filter(continuous_feedback::Column::TenantId.eq(tenant_id))
        .filter(continuous_feedback::Column::RevieweeEmployeeId.eq(participant.employee_id))
        .filter(
            continuous_feedback::Column::ReviewCycleId
                .eq(participant.review_cycle_id)
                .or(continuous_feedback::Column::ReviewCycleId.is_null()),
        )
        .filter(continuous_feedback::Column::Visibility.eq("EMPLOYEE_VISIBLE"))
        .order_by_desc(continuous_feedback::Column::ObservationDate)
        .all(db)
        .await?
        .into_iter()
        .map(PerformanceFeedbackDto::from)
        .collect();
    let answers = appraisal_answer::Entity::find()
        .filter(appraisal_answer::Column::TenantId.eq(tenant_id))
        .filter(appraisal_answer::Column::PerformanceParticipantId.eq(participant.id))
        .filter(appraisal_answer::Column::Revision.eq(participant.response_revision))
        .all(db)
        .await?
        .into_iter()
        .map(AppraisalAnswerDto::from)
        .collect();
    let template = load_template(db, tenant_id, participant.appraisal_template_id).await?;
    Ok(PerformanceReviewDetailDto { review, goals, feedback, template, answers })
}

pub async fn program_for_cycle<C>(
    db: &C,
    tenant_id: Uuid,
    cycle_id: Uuid,
) -> KabiPayResult<performance_program::Model>
where
    C: ConnectionTrait,
{
    let statement = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT p.* FROM performance_program p JOIN review_cycle c ON c.performance_program_id = p.id AND c.tenant_id = p.tenant_id WHERE p.tenant_id = $1 AND c.id = $2",
        [tenant_id.into(), cycle_id.into()],
    );
    performance_program::Model::find_by_statement(statement)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance program",
            id: cycle_id.to_string(),
        })
}

pub async fn cycle_stage<C>(
    db: &C,
    tenant_id: Uuid,
    cycle_id: Uuid,
) -> KabiPayResult<String>
where
    C: ConnectionTrait,
{
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT current_stage FROM review_cycle WHERE tenant_id = $1 AND id = $2",
            [tenant_id.into(), cycle_id.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "review cycle",
            id: cycle_id.to_string(),
        })?;
    row.try_get("", "current_stage").map_err(KabiPayError::from)
}
