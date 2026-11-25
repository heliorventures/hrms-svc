//! Authorization resolves effective installed review authority while historical
//! participant reviewer fields remain source audit snapshots.
use kabipay_common::KabiPayResult;
use kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::performance_participant;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;
pub async fn apply_installed_manager<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    participant: &mut performance_participant::Model,
) -> KabiPayResult<()> {
    participant.manager_employee_id=effective_review_manager(db,tenant,participant.id,participant.employee_id,participant.manager_employee_id).await?;
    Ok(())
}

pub async fn effective_review_manager<C: ConnectionTrait>(db:&C,tenant:Uuid,participant_id:Uuid,subject_employee_id:Uuid,fallback:Option<Uuid>)->KabiPayResult<Option<Uuid>> {
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT a.manager_employee_id FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.status='ACTIVE' AND a.relationship_type IN ('PRIMARY','REVIEWER') AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND EXISTS(SELECT 1 FROM assignment_capability c WHERE c.tenant_id=a.tenant_id AND c.assignment_id=a.id AND c.capability_code='performance:evaluate') ORDER BY CASE WHEN a.relationship_type='PRIMARY' THEN 0 ELSE 1 END,a.graph_version DESC,a.id LIMIT 1",[tenant.into(),subject_employee_id.into()])).await?;
    if let Some(row) = row {
        return Ok(row.try_get("", "manager_employee_id")?);
    } else {
        let current=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT manager_employee_id FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id=$2 AND relationship_type='PRIMARY' AND status='ACTIVE' AND effective_from<=clock_timestamp() AND (effective_to IS NULL OR effective_to>clock_timestamp()) UNION ALL SELECT e.reporting_manager_id AS manager_employee_id FROM employee e WHERE e.tenant_id=$1 AND e.id=$2 AND (organization_reporting_owned($1) OR EXISTS(SELECT 1 FROM organization_review_manager s WHERE s.tenant_id=$1 AND s.participant_id=$3) OR EXISTS(SELECT 1 FROM management_assignment history WHERE history.tenant_id=$1 AND history.subject_employee_id=$2 AND history.relationship_type='PRIMARY')) AND NOT EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()))",[tenant.into(),subject_employee_id.into(),participant_id.into()])).await?;
        if let Some(current)=current {return Ok(current.try_get("","manager_employee_id")?);}
    }
    Ok(fallback)
}

/// The same exact current assignment guard is used with a business row lock
/// for decisions and without an update-requiring lock for mail eligibility.
pub const REVIEW_MANAGER_ASSIGNMENT_SQL:&str=r#"SELECT 1 AS n FROM organization_review_manager s WHERE s.tenant_id=$1 AND s.participant_id=$2 AND s.principal_id=$3 AND s.assignment_id=$4 AND s.assignment_version=$5 AND s.assignment_id=(SELECT a.id FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$6 AND a.status='ACTIVE' AND a.relationship_type IN ('PRIMARY','REVIEWER') AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND EXISTS(SELECT 1 FROM assignment_capability c WHERE c.tenant_id=a.tenant_id AND c.assignment_id=a.id AND c.capability_code='performance:evaluate') ORDER BY CASE WHEN a.relationship_type='PRIMARY' THEN 0 ELSE 1 END,a.graph_version DESC,a.id LIMIT 1)"#;
#[allow(clippy::too_many_arguments)]
pub async fn assert_current_review_assignment<C:ConnectionTrait>(db:&C,tenant:Uuid,participant:Uuid,principal:Uuid,assignment:Uuid,version:i64,subject:Uuid,lock:bool)->KabiPayResult<()> {
 let query=format!("{}{}",REVIEW_MANAGER_ASSIGNMENT_SQL,if lock{" FOR SHARE OF s"}else{""});
 let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,query,[tenant.into(),participant.into(),principal.into(),assignment.into(),version.into(),subject.into()])).await?;
 if row.is_none(){return Err(kabipay_organization_tenant::unavailable());}
 Ok(())
}
