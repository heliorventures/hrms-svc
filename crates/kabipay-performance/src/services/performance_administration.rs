//! Transactional persistence for performance administration.  Every caller acquires the
//! cycle lock before the participant lock; this module never reverses that order.

use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement, TryGetable};
use uuid::Uuid;

use crate::services::performance_lifecycle;

fn value_error(message: impl Into<String>) -> KabiPayError {
    KabiPayError::Validation(message.into())
}

pub async fn audit_event(
    txn: &DatabaseTransaction, tenant_id: Uuid, cycle_id: Option<Uuid>, participant_id: Option<Uuid>,
    event_type: &str, reason: Option<&str>, actor_id: Option<Uuid>,
) -> KabiPayResult<()> {
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "INSERT INTO performance_audit_event (id,tenant_id,review_cycle_id,performance_participant_id,event_type,reason,actor_user_id,created_at) VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,$6,NOW())",
        [tenant_id.into(), cycle_id.into(), participant_id.into(), event_type.into(), reason.into(), actor_id.into()],
    )).await?;
    Ok(())
}

pub async fn audit_cycle_transition(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    cycle_id: Uuid,
    from_stage: &str,
    to_stage: &str,
    actor_id: Option<Uuid>,
    source: &str,
) -> KabiPayResult<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO performance_audit_event (id,tenant_id,review_cycle_id,event_type,actor_user_id,metadata,created_at) VALUES (gen_random_uuid(),$1,$2,'CYCLE_ADVANCED',$3,jsonb_build_object('fromStage',$4,'toStage',$5,'source',$6),NOW())",
        [tenant_id.into(), cycle_id.into(), actor_id.into(), from_stage.into(), to_stage.into(), source.into()],
    )).await?;
    Ok(())
}

pub async fn audit_program_event(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    program_id: Uuid,
    event_type: &str,
    reason: &str,
    actor_id: Uuid,
) -> KabiPayResult<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO performance_audit_event (id,tenant_id,event_type,reason,actor_user_id,metadata,created_at) VALUES (gen_random_uuid(),$1,$2,$3,$4,jsonb_build_object('programId',$5),NOW())",
        [tenant_id.into(), event_type.into(), reason.into(), actor_id.into(), program_id.into()],
    )).await?;
    Ok(())
}

/// Persist a complete metadata/KPI snapshot for the current revision before changing it.
/// Appraisal answers are already revisioned in `appraisal_answer` and remain immutable.
pub async fn snapshot_revision(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> KabiPayResult<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO performance_participant_revision
          (id,tenant_id,performance_participant_id,revision,self_submitted_at,manager_submitted_at,
           acknowledged_at,acknowledgement_comment,manager_rating,manager_performance_band,
           final_rating,performance_band,calibration_provenance,kpi_snapshot,created_at)
          SELECT gen_random_uuid(),p.tenant_id,p.id,p.response_revision,p.self_submitted_at,p.manager_submitted_at,
                 p.acknowledged_at,p.acknowledgement_comment,p.manager_rating,p.manager_performance_band,
                 p.final_rating,p.performance_band,p.calibration_provenance,
                 COALESCE((SELECT jsonb_agg(jsonb_build_object('id',k.id,'goalId',k.goal_id,'metricName',k.metric_name,
                   'targetValue',k.target_value::text,'actualValue',k.actual_value::text,'unit',k.unit,'evidence',k.evidence,
                   'comment',k.comment,'measurementDate',k.measurement_date) ORDER BY k.id)
                   FROM kpi k JOIN goal g ON g.id=k.goal_id AND g.tenant_id=k.tenant_id
                   WHERE k.tenant_id=p.tenant_id AND g.review_cycle_id=p.review_cycle_id AND g.employee_id=p.employee_id),'[]'::jsonb),
                 NOW()
          FROM performance_participant p WHERE p.tenant_id=$1 AND p.id=$2
          ON CONFLICT (performance_participant_id,revision) DO UPDATE SET
            self_submitted_at=EXCLUDED.self_submitted_at,manager_submitted_at=EXCLUDED.manager_submitted_at,
            acknowledged_at=EXCLUDED.acknowledged_at,acknowledgement_comment=EXCLUDED.acknowledgement_comment,
            manager_rating=EXCLUDED.manager_rating,manager_performance_band=EXCLUDED.manager_performance_band,
            final_rating=EXCLUDED.final_rating,performance_band=EXCLUDED.performance_band,
            calibration_provenance=EXCLUDED.calibration_provenance,kpi_snapshot=EXCLUDED.kpi_snapshot"#,
        [tenant_id.into(), participant_id.into()],
    )).await?;
    Ok(())
}

pub async fn set_calibration(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    participant_id: Uuid,
    expected_revision: i32,
    final_rating: rust_decimal::Decimal,
    performance_band: Option<String>,
    reason: &str,
    actor_id: Uuid,
) -> KabiPayResult<()> {
    let cycle_id: Uuid = txn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT review_cycle_id FROM performance_participant WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), participant_id.into()],
    )).await?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() })?.try_get("", "review_cycle_id")?;
    let cycle = txn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT c.current_stage,p.rating_min,p.rating_max FROM review_cycle c JOIN performance_program p ON p.id=c.performance_program_id AND p.tenant_id=c.tenant_id WHERE c.tenant_id=$1 AND c.id=$2 FOR UPDATE OF c",
        [tenant_id.into(), cycle_id.into()],
    )).await?.ok_or_else(|| KabiPayError::NotFound { entity: "performance cycle", id: cycle_id.to_string() })?;
    let stage: String = cycle.try_get("", "current_stage")?;
    let rating_min: rust_decimal::Decimal = cycle.try_get("", "rating_min")?;
    let rating_max: rust_decimal::Decimal = cycle.try_get("", "rating_max")?;
    if stage != "HR_CALIBRATION" { return Err(value_error("Calibration is only available during HR calibration")); }
    if reason.trim().is_empty() { return Err(value_error("Calibration reason is required")); }
    performance_lifecycle::validate_rating(final_rating, rating_min, rating_max).map_err(value_error)?;
    let row = txn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT review_cycle_id,response_revision,manager_submitted_at,is_excluded FROM performance_participant WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        [tenant_id.into(), participant_id.into()],
    )).await?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() })?;
    let revision: i32 = row.try_get("", "response_revision")?;
    let manager_submitted_at: Option<DateTime<Utc>> = row.try_get("", "manager_submitted_at")?;
    let locked_cycle_id: Uuid = row.try_get("", "review_cycle_id")?;
    let excluded: bool = row.try_get("", "is_excluded")?;
    if locked_cycle_id != cycle_id { return Err(KabiPayError::Conflict("Performance review changed while acquiring locks; retry".into())); }
    if excluded { return Err(value_error("Excluded participants cannot be calibrated")); }
    if revision != expected_revision { return Err(KabiPayError::Conflict("Performance review revision changed; refresh and retry".into())); }
    if manager_submitted_at.is_none() { return Err(value_error("Manager assessment is required before calibration")); }
    snapshot_revision(txn, tenant_id, participant_id).await?;
    let now = Utc::now();
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE performance_participant SET final_rating=$3,performance_band=$4,calibration_provenance='HR_CALIBRATED',updated_at=$5 WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), participant_id.into(), final_rating.into(), performance_band.clone().into(), now.into()],
    )).await?;
    audit_event(txn, tenant_id, Some(cycle_id), Some(participant_id), "CALIBRATION_SAVED", Some(reason), Some(actor_id)).await?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE performance_participant_revision SET final_rating=$3,performance_band=$4,calibration_reason=$5,calibrated_by_user_id=$6,calibrated_at=$7,calibration_provenance='HR_CALIBRATED' WHERE tenant_id=$1 AND performance_participant_id=$2 AND revision=$8",
        [tenant_id.into(), participant_id.into(), final_rating.into(), performance_band.clone().into(), reason.into(), actor_id.into(), now.into(), revision.into()],
    )).await?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "INSERT INTO performance_calibration_decision (id,tenant_id,performance_participant_id,revision,final_rating,performance_band,reason,decided_by_user_id,decided_at) VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,$6,$7,$8)",
        [tenant_id.into(), participant_id.into(), revision.into(), final_rating.into(), performance_band.into(), reason.into(), actor_id.into(), now.into()],
    )).await?;
    Ok(())
}

pub async fn reopen_revision(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    participant_id: Uuid,
    expected_revision: i32,
    correction_stage: &str,
    reason: &str,
    actor_id: Uuid,
) -> KabiPayResult<i32> {
    let cycle_id: Uuid = txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "SELECT review_cycle_id FROM performance_participant WHERE tenant_id=$1 AND id=$2", [tenant_id.into(), participant_id.into()]))
        .await?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() })?.try_get("", "review_cycle_id")?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "SELECT id FROM review_cycle WHERE tenant_id=$1 AND id=$2 FOR UPDATE", [tenant_id.into(), cycle_id.into()])).await?;
    let row = txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "SELECT review_cycle_id,response_revision,self_submitted_at,manager_submitted_at,is_excluded FROM performance_participant WHERE tenant_id=$1 AND id=$2 FOR UPDATE", [tenant_id.into(), participant_id.into()]))
        .await?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() })?;
    let revision: i32 = row.try_get("", "response_revision")?;
    let self_submitted: Option<DateTime<Utc>> = row.try_get("", "self_submitted_at")?;
    let manager_submitted: Option<DateTime<Utc>> = row.try_get("", "manager_submitted_at")?;
    let locked_cycle_id: Uuid = row.try_get("", "review_cycle_id")?;
    let excluded: bool = row.try_get("", "is_excluded")?;
    if locked_cycle_id != cycle_id { return Err(KabiPayError::Conflict("Performance review changed while acquiring locks; retry".into())); }
    if excluded { return Err(value_error("Excluded participants cannot be reopened")); }
    if revision != expected_revision { return Err(KabiPayError::Conflict("Performance review revision changed; refresh and retry".into())); }
    match correction_stage {
        "SELF_REVIEW" if self_submitted.is_none() => return Err(value_error("A self submission is required before reopening self review")),
        "MANAGER_REVIEW" if self_submitted.is_none() || manager_submitted.is_none() => return Err(value_error("Submitted self and manager reviews are required before reopening manager review")),
        "SELF_REVIEW" | "MANAGER_REVIEW" => {},
        _ => return Err(value_error("Correction stage must be SELF_REVIEW or MANAGER_REVIEW")),
    }
    snapshot_revision(txn, tenant_id, participant_id).await?;
    let next_revision = revision.checked_add(1).ok_or_else(|| value_error("Performance review revision limit reached"))?;
    if correction_stage == "SELF_REVIEW" || correction_stage == "MANAGER_REVIEW" {
        txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO appraisal_answer (id,tenant_id,performance_participant_id,question_id,revision,employee_text_answer,employee_selected_option_ids,self_rating,manager_text_answer,manager_selected_option_ids,manager_rating,created_at,updated_at) SELECT gen_random_uuid(),tenant_id,performance_participant_id,question_id,$3,employee_text_answer,employee_selected_option_ids,self_rating,CASE WHEN $4='MANAGER_REVIEW' THEN manager_text_answer ELSE NULL END,CASE WHEN $4='MANAGER_REVIEW' THEN manager_selected_option_ids ELSE NULL END,CASE WHEN $4='MANAGER_REVIEW' THEN manager_rating ELSE NULL END,$5,$5 FROM appraisal_answer WHERE tenant_id=$1 AND performance_participant_id=$2 AND revision=$6",
            [tenant_id.into(), participant_id.into(), next_revision.into(), correction_stage.into(), Utc::now().into(), revision.into()])).await?;
    }
    let now = Utc::now();
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE performance_participant SET response_revision=$3,self_submitted_at=CASE WHEN $4='MANAGER_REVIEW' THEN self_submitted_at ELSE NULL END,manager_submitted_at=NULL,acknowledged_at=NULL,acknowledgement_comment=NULL,manager_rating=NULL,manager_performance_band=NULL,final_rating=NULL,performance_band=NULL,calibration_provenance=NULL,status=$4,updated_at=$5 WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), participant_id.into(), next_revision.into(), correction_stage.into(), now.into()])).await?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "INSERT INTO performance_participant_revision (id,tenant_id,performance_participant_id,revision,correction_stage,reopen_reason,reopened_by_user_id,reopened_at,self_submitted_at,created_at) VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,$6,$7,CASE WHEN $4='MANAGER_REVIEW' THEN $8 ELSE NULL END,$7)",
        [tenant_id.into(), participant_id.into(), next_revision.into(), correction_stage.into(), reason.into(), actor_id.into(), now.into(), self_submitted.into()])).await?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE review_cycle SET current_stage=$3,status='ACTIVE',updated_at=$4 WHERE tenant_id=$1 AND id=$2", [tenant_id.into(), cycle_id.into(), correction_stage.into(), now.into()])).await?;
    audit_event(txn, tenant_id, Some(cycle_id), Some(participant_id), "REVIEW_REOPENED", Some(reason), Some(actor_id)).await?;
    Ok(next_revision)
}

pub async fn create_or_keep_exception(txn: &DatabaseTransaction, tenant_id: Uuid, cycle_id: Uuid, program_id: Option<Uuid>, code: &str, details: &str) -> KabiPayResult<()> {
    let key = format!("{cycle_id}:{code}");
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "INSERT INTO performance_admin_exception (id,tenant_id,performance_program_id,review_cycle_id,exception_code,details,dedup_key,created_at) VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,$6,NOW()) ON CONFLICT DO NOTHING",
        [tenant_id.into(),program_id.into(),cycle_id.into(),code.into(),details.into(),key.into()])).await?;
    Ok(())
}

const NO_ELIGIBLE_PARTICIPANTS: &str = "NO_ELIGIBLE_PARTICIPANTS";
const NO_ELIGIBLE_PARTICIPANTS_DETAILS: &str =
    "This cycle has no participant snapshots and cannot advance.";

/// Call after acquiring the cycle lock. Empty persisted cycles must not pass
/// stage prerequisites merely because every missing-submission count is zero.
/// Older launch exceptions have no dedup key, so reuse one before inserting a
/// deduplicated replacement.
async fn block_empty_cycle(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    cycle_id: Uuid,
    program_id: Option<Uuid>,
) -> KabiPayResult<bool> {
    let participant_count: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS participant_count FROM performance_participant WHERE tenant_id=$1 AND review_cycle_id=$2",
            [tenant_id.into(), cycle_id.into()],
        ))
        .await?
        .ok_or_else(|| value_error("Performance cycle disappeared"))?
        .try_get("", "participant_count")?;
    if participant_count != 0 {
        return Ok(false);
    }
    let existing = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM performance_admin_exception WHERE tenant_id=$1 AND review_cycle_id=$2 AND exception_code=$3 AND resolved_at IS NULL LIMIT 1 FOR UPDATE",
            [tenant_id.into(), cycle_id.into(), NO_ELIGIBLE_PARTICIPANTS.into()],
        ))
        .await?;
    if existing.is_none() {
        create_or_keep_exception(
            txn,
            tenant_id,
            cycle_id,
            program_id,
            NO_ELIGIBLE_PARTICIPANTS,
            NO_ELIGIBLE_PARTICIPANTS_DETAILS,
        )
        .await?;
    }
    Ok(true)
}

fn exception_code_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "GOAL_SETTING" => Some("GOAL_APPROVAL_PENDING"),
        "SELF_REVIEW" => Some("SELF_REVIEW_PENDING"),
        "MANAGER_REVIEW" => Some("MANAGER_REVIEW_PENDING"),
        "HR_CALIBRATION" => Some("CALIBRATION_PENDING"),
        "EMPLOYEE_ACKNOWLEDGEMENT" => Some("ACKNOWLEDGEMENT_PENDING"),
        _ => None,
    }
}

/// Retry only the currently actionable exception.  A resolved or stale row cannot advance a
/// later stage; callers read the persisted row after commit to report its real resolution state.
pub async fn retry_exception(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    exception_id: Uuid,
    actor_id: Uuid,
) -> KabiPayResult<()> {
    let exception = txn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT review_cycle_id,exception_code,resolved_at FROM performance_admin_exception WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        [tenant_id.into(), exception_id.into()],
    )).await?.ok_or_else(|| KabiPayError::NotFound {
        entity: "performance exception",
        id: exception_id.to_string(),
    })?;
    let cycle_id: Option<Uuid> = exception.try_get("", "review_cycle_id")?;
    let cycle_id = cycle_id.ok_or_else(|| value_error("This exception has no retryable performance cycle"))?;
    let resolved_at: Option<DateTime<Utc>> = exception.try_get("", "resolved_at")?;
    if resolved_at.is_some() {
        return Err(value_error("This performance exception has already been resolved"));
    }
    let exception_code: String = exception.try_get("", "exception_code")?;
    if exception_code == NO_ELIGIBLE_PARTICIPANTS {
        advance_cycle_with_context(
            txn,
            tenant_id,
            cycle_id,
            &exception_code,
            Some(actor_id),
            "RETRY",
        )
        .await?;
        return Ok(());
    }
    let cycle = txn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT current_stage FROM review_cycle WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), cycle_id.into()],
    )).await?.ok_or_else(|| KabiPayError::NotFound {
        entity: "performance cycle",
        id: cycle_id.to_string(),
    })?;
    let stage: String = cycle.try_get("", "current_stage")?;
    if exception_code_for_stage(&stage) != Some(exception_code.as_str()) {
        return Err(value_error("This performance exception is stale and cannot advance the current stage"));
    }
    advance_cycle_with_context(txn, tenant_id, cycle_id, &exception_code, Some(actor_id), "RETRY").await?;
    Ok(())
}

/// Advance exactly one stage after locking the cycle. Manual requests, deadline sweeps and
/// exception retry all call this operation so their prerequisite and deduplication behavior is identical.
pub async fn advance_cycle(txn: &DatabaseTransaction, tenant_id: Uuid, cycle_id: Uuid) -> KabiPayResult<String> {
    advance_cycle_with_context(txn, tenant_id, cycle_id, "", None, "MANUAL").await
}

pub async fn advance_cycle_with_context(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    cycle_id: Uuid,
    expected_exception_code: &str,
    actor_id: Option<Uuid>,
    source: &str,
) -> KabiPayResult<String> {
    advance_cycle_with_due_date(txn, tenant_id, cycle_id, expected_exception_code, actor_id, source, None).await
}

/// Scheduler callers provide `business_date`; it is checked after taking the cycle lock so a
/// concurrent poll cannot advance the newly-entered stage using a stale pre-lock selection.
pub async fn advance_due_cycle(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    cycle_id: Uuid,
    business_date: NaiveDate,
) -> KabiPayResult<String> {
    advance_cycle_with_due_date(txn, tenant_id, cycle_id, "", None, "SCHEDULER", Some(business_date)).await
}

async fn advance_cycle_with_due_date(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    cycle_id: Uuid,
    expected_exception_code: &str,
    actor_id: Option<Uuid>,
    source: &str,
    business_date: Option<NaiveDate>,
) -> KabiPayResult<String> {
    let row = txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "SELECT c.current_stage,c.performance_program_id AS program_id,COALESCE(p.include_calibration,FALSE) AS include_calibration,COALESCE(p.include_acknowledgement,FALSE) AS include_acknowledgement,COALESCE(p.goal_weight_required,100) AS goal_weight_required,CASE c.current_stage WHEN 'GOAL_SETTING' THEN c.goal_setting_due_date WHEN 'SELF_REVIEW' THEN c.self_review_due_date WHEN 'MANAGER_REVIEW' THEN c.manager_review_due_date WHEN 'HR_CALIBRATION' THEN c.calibration_due_date WHEN 'EMPLOYEE_ACKNOWLEDGEMENT' THEN c.acknowledgement_due_date ELSE NULL END AS current_due_date FROM review_cycle c LEFT JOIN performance_program p ON p.id=c.performance_program_id AND p.tenant_id=c.tenant_id WHERE c.tenant_id=$1 AND c.id=$2 FOR UPDATE OF c", [tenant_id.into(),cycle_id.into()]))
        .await?.ok_or_else(|| KabiPayError::NotFound { entity:"performance cycle",id:cycle_id.to_string() })?;
    let stage: String=row.try_get("","current_stage")?; let program_id:Option<Uuid>=row.try_get("","program_id")?; let calibrate:bool=row.try_get("","include_calibration")?;let acknowledge:bool=row.try_get("","include_acknowledgement")?;let required_weight:rust_decimal::Decimal=row.try_get("","goal_weight_required")?;
    let empty_cycle = block_empty_cycle(txn, tenant_id, cycle_id, program_id).await?;
    if empty_cycle {
        return Ok("BLOCKED".into());
    }
    if expected_exception_code == NO_ELIGIBLE_PARTICIPANTS {
        return Err(value_error("This empty-population exception is stale and cannot advance the cycle"));
    }
    if !expected_exception_code.is_empty() && exception_code_for_stage(&stage) != Some(expected_exception_code) {
        return Err(value_error("This performance exception is stale and cannot advance the current stage"));
    }
    if let Some(business_date) = business_date {
        let due_date: Option<NaiveDate> = row.try_get("", "current_due_date")?;
        if due_date.map(|due_date| due_date > business_date).unwrap_or(true) {
            return Ok("NOT_DUE".into());
        }
    }
    let (required_column, code, details) = match stage.as_str() {
        "GOAL_SETTING" => ("goal", "GOAL_APPROVAL_PENDING", "All included participants require approved goals totaling the configured weight before self review."),
        "SELF_REVIEW" => ("self_submitted_at", "SELF_REVIEW_PENDING", "All included participants must submit self review before manager review."),
        "MANAGER_REVIEW" => ("manager_submitted_at", "MANAGER_REVIEW_PENDING", "All included participants must submit manager review before the next stage."),
        "HR_CALIBRATION" => ("calibrated", "CALIBRATION_PENDING", "An HR calibration decision is required for every included participant revision."),
        "EMPLOYEE_ACKNOWLEDGEMENT" => ("acknowledged_at", "ACKNOWLEDGEMENT_PENDING", "All included participants must acknowledge before the cycle closes."),
        "CLOSED" => return Err(value_error("A closed performance cycle cannot be advanced")),
        _ => return Err(value_error("Performance cycle stage is invalid")),
    };
    let missing: i64 = match required_column {
        "goal" => txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT count(*) AS missing FROM performance_participant p WHERE p.tenant_id=$1 AND p.review_cycle_id=$2 AND NOT p.is_excluded AND NOT EXISTS (SELECT 1 FROM goal g WHERE g.tenant_id=p.tenant_id AND g.review_cycle_id=p.review_cycle_id AND g.employee_id=p.employee_id GROUP BY g.review_cycle_id,g.employee_id HAVING count(*)>0 AND count(g.weightage)=count(*) AND bool_and(g.status='APPROVED') AND sum(g.weightage)=$3)", [tenant_id.into(),cycle_id.into(),required_weight.into()])).await?.ok_or_else(|| value_error("Performance cycle disappeared"))?.try_get("","missing")?,
        "calibrated" => txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT count(*) AS missing FROM performance_participant p LEFT JOIN performance_participant_revision r ON r.performance_participant_id=p.id AND r.revision=p.response_revision WHERE p.tenant_id=$1 AND p.review_cycle_id=$2 AND NOT p.is_excluded AND r.calibrated_at IS NULL", [tenant_id.into(),cycle_id.into()])).await?.ok_or_else(|| value_error("Performance cycle disappeared"))?.try_get("","missing")?,
        column => txn.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            &format!("SELECT count(*) AS missing FROM performance_participant WHERE tenant_id=$1 AND review_cycle_id=$2 AND NOT is_excluded AND {column} IS NULL"), [tenant_id.into(),cycle_id.into()])).await?.ok_or_else(|| value_error("Performance cycle disappeared"))?.try_get("","missing")?,
    };
    if missing > 0 {
        create_or_keep_exception(txn, tenant_id, cycle_id, program_id, code, details).await?;
        return Ok("BLOCKED".into());
    }
    let next = match stage.as_str() {
        "GOAL_SETTING" => "SELF_REVIEW", "SELF_REVIEW" => "MANAGER_REVIEW",
        "MANAGER_REVIEW" if calibrate => "HR_CALIBRATION", "MANAGER_REVIEW" if acknowledge => "EMPLOYEE_ACKNOWLEDGEMENT", "MANAGER_REVIEW" => "CLOSED",
        "HR_CALIBRATION" if acknowledge => "EMPLOYEE_ACKNOWLEDGEMENT", "HR_CALIBRATION" => "CLOSED",
        "EMPLOYEE_ACKNOWLEDGEMENT" => "CLOSED", _ => unreachable!(),
    };
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"UPDATE review_cycle SET current_stage=$3,status=CASE WHEN $3='CLOSED' THEN 'COMPLETED' ELSE status END,updated_at=NOW() WHERE tenant_id=$1 AND id=$2",[tenant_id.into(),cycle_id.into(),next.into()])).await?;
    txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"UPDATE performance_participant SET status=$3,updated_at=NOW() WHERE tenant_id=$1 AND review_cycle_id=$2 AND NOT is_excluded",[tenant_id.into(),cycle_id.into(),next.into()])).await?;
    audit_cycle_transition(txn, tenant_id, cycle_id, &stage, next, actor_id, source).await?;
    let key=format!("{cycle_id}:{code}"); txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,"UPDATE performance_admin_exception SET resolved_at=NOW() WHERE tenant_id=$1 AND dedup_key=$2 AND resolved_at IS NULL",[tenant_id.into(),key.into()])).await?;
    Ok(next.into())
}
