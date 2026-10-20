//! Resumable source snapshots. One bounded transaction per batch; concurrent
//! domain writes emit their own higher versions through source triggers.
use crate::statement;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, TransactionTrait};
use uuid::Uuid;
pub async fn queue_bootstrap(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    organization: Uuid,
) -> KabiPayResult<()> {
    tx.execute(statement("INSERT INTO organization_fact_bootstrap(tenant_id,organization_id) VALUES($1,$2) ON CONFLICT DO NOTHING",vec![tenant.into(),organization.into()])).await?;
    Ok(())
}
pub async fn bootstrap_once(
    db: &DatabaseConnection,
    tenant: Uuid,
    limit: i64,
) -> KabiPayResult<bool> {
    if !(1..=200).contains(&limit) {
        return Err(KabiPayError::Validation(
            "Invalid source bootstrap batch size".into(),
        ));
    }
    let tx = db.begin().await?;
    let Some(candidate)=tx.query_one(statement("SELECT b.organization_id,m.membership_id,m.version FROM organization_fact_bootstrap b JOIN organization_membership_installation m ON m.tenant_id=b.tenant_id AND m.organization_id=b.organization_id AND m.status='ACTIVE' WHERE b.tenant_id=$1 AND b.status='PENDING' ORDER BY b.updated_at,b.organization_id LIMIT 1",vec![tenant.into()])).await? else {tx.commit().await?;return Ok(false)};
    let organization: Uuid = candidate.try_get("", "organization_id")?;
    crate::membership_lifecycle::require_active(
        &tx,
        tenant,
        candidate.try_get("", "membership_id")?,
        organization,
        candidate.try_get("", "version")?,
    )
    .await?;
    let Some(job)=tx.query_one(statement("SELECT phase,after_id FROM organization_fact_bootstrap WHERE tenant_id=$1 AND organization_id=$2 AND status='PENDING' FOR UPDATE SKIP LOCKED",vec![tenant.into(),organization.into()])).await? else {tx.commit().await?;return Ok(false)};
    let phase: i32 = job.try_get("", "phase")?;
    let cursor: Option<Uuid> = job.try_get("", "after_id")?;
    let (table, kind) = match phase {
        0 => ("employee", "employee"),
        1 => ("leave_request", "leave_request"),
        2 => ("timesheet_week_batch", "timesheet"),
        3 => ("timesheet_entry", "timesheet_entry"),
        4 => ("performance_participant", "performance_participant"),
        _ => {
            tx.execute(statement("UPDATE organization_fact_bootstrap SET status='COMPLETE',updated_at=now() WHERE tenant_id=$1 AND organization_id=$2",vec![tenant.into(),organization.into()])).await?;
            tx.commit().await?;
            return Ok(true);
        }
    };
    // Table and kind are compile-time whitelists, not configuration or payload.
    let fields=match kind {"employee"=>"jsonb_build_object('id',s.id,'tenant_id',s.tenant_id,'first_name',s.first_name,'last_name',s.last_name,'status',s.status,'is_deleted',s.is_deleted)","timesheet_entry"=>"jsonb_build_object('id',s.id,'tenant_id',s.tenant_id,'employee_id',s.employee_id,'work_date',s.work_date,'hours_worked',s.hours_worked,'status',s.status,'is_deleted',s.is_deleted)","performance_participant"=>"jsonb_build_object('id',s.id,'tenant_id',s.tenant_id,'employee_id',s.employee_id,'status',s.status,'is_excluded',s.is_excluded)","leave_request"=>"jsonb_build_object('id',s.id,'tenant_id',s.tenant_id,'employee_id',s.employee_id,'status',s.status,'is_deleted',s.is_deleted)",_=>"jsonb_build_object('id',s.id,'tenant_id',s.tenant_id,'employee_id',s.employee_id,'status',s.status)"};
    let rows=tx.query_all(statement(&format!("SELECT s.id,{fields} AS body,v.version,v.last_actor,v.fact_occurred_at FROM {table} s JOIN organization_resource_version v ON v.tenant_id=s.tenant_id AND v.resource_id=s.id AND v.resource_type=$2 WHERE s.tenant_id=$1 AND ($3::uuid IS NULL OR s.id>$3) ORDER BY s.id LIMIT $4"),vec![tenant.into(),kind.into(),cursor.into(),limit.into()])).await?;
    let mut last = None;
    let count = rows.len();
    for row in rows {
        let mut body: serde_json::Value = row.try_get("", "body")?;
        body.as_object_mut().ok_or_else(crate::unavailable)?.insert(
            "__source_actor".into(),
            row.try_get::<serde_json::Value>("", "last_actor")?,
        );
        body.as_object_mut().ok_or_else(crate::unavailable)?.insert(
            "__source_occurred_at".into(),
            serde_json::to_value(
                row.try_get::<chrono::DateTime<chrono::Utc>>("", "fact_occurred_at")?,
            )?,
        );
        let version: i64 = row.try_get("", "version")?;
        last = Some(row.try_get::<Uuid>("", "id")?);
        let deleted = body
            .get("is_deleted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || (kind == "performance_participant"
                && body.get("is_excluded").and_then(serde_json::Value::as_bool) == Some(true));
        tx.execute(statement(
            "SELECT organization_emit_fact($1,$2,$3,$4,$5)",
            vec![
                body.into(),
                kind.into(),
                version.into(),
                organization.into(),
                deleted.into(),
            ],
        ))
        .await?;
    }
    let next_phase = if count < limit as usize {
        phase + 1
    } else {
        phase
    };
    let after = if next_phase != phase { None } else { last };
    tx.execute(statement("UPDATE organization_fact_bootstrap SET phase=$3,after_id=$4,status=CASE WHEN $3=5 THEN 'COMPLETE' ELSE 'PENDING' END,updated_at=now() WHERE tenant_id=$1 AND organization_id=$2",vec![tenant.into(),organization.into(),next_phase.into(),after.into()])).await?;
    tx.commit().await?;
    Ok(true)
}
