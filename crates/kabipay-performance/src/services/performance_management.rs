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
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT a.manager_employee_id FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.status='ACTIVE' AND a.relationship_type IN ('PRIMARY','REVIEWER') AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND EXISTS(SELECT 1 FROM assignment_capability c WHERE c.tenant_id=a.tenant_id AND c.assignment_id=a.id AND c.capability_code='performance:evaluate') ORDER BY CASE WHEN a.relationship_type='PRIMARY' THEN 0 ELSE 1 END,a.graph_version DESC,a.id LIMIT 1",[tenant.into(),participant.employee_id.into()])).await?;
    if let Some(row) = row {
        participant.manager_employee_id = row.try_get("", "manager_employee_id")?;
    } else {
        let current=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT manager_employee_id FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id=$2 AND relationship_type='PRIMARY' AND status='ACTIVE' AND effective_from<=clock_timestamp() AND (effective_to IS NULL OR effective_to>clock_timestamp()) UNION ALL SELECT e.reporting_manager_id AS manager_employee_id FROM employee e WHERE e.tenant_id=$1 AND e.id=$2 AND (organization_reporting_owned($1) OR EXISTS(SELECT 1 FROM organization_review_manager s WHERE s.tenant_id=$1 AND s.participant_id=$3) OR EXISTS(SELECT 1 FROM management_assignment history WHERE history.tenant_id=$1 AND history.subject_employee_id=$2 AND history.relationship_type='PRIMARY')) AND NOT EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()))",[tenant.into(),participant.employee_id.into(),participant.id.into()])).await?;
        if let Some(current)=current {participant.manager_employee_id=current.try_get("","manager_employee_id")?;}
    }
    Ok(())
}
