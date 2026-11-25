//! Finalization-only producer. Historic import/draft/export callers do not use it.
use kabipay_common::{important_email::*, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;
pub async fn emit_finalized_payslip_emails(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    cycle: Uuid,
    actor: Option<EmailActorRef>,
) -> KabiPayResult<usize> {
    let rows = tx
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT p.id,p.updated_at,e.user_id FROM payslip p
 JOIN payroll_cycle c ON c.tenant_id=p.tenant_id AND c.id=p.payroll_cycle_id
 JOIN employee e ON e.tenant_id=p.tenant_id AND e.id=p.employee_id AND NOT e.is_deleted
 JOIN "user" u ON u.tenant_id=e.tenant_id AND u.id=e.user_id AND u.is_active AND NOT u.is_deleted
 WHERE p.tenant_id=$1 AND p.payroll_cycle_id=$2 AND p.status='GENERATED' AND c.status='PROCESSED'
 AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE') ORDER BY p.id"#,
            [tenant.into(), cycle.into()],
        ))
        .await?;
    let mut count = 0;
    for row in rows {
        let at = row.try_get("", "updated_at")?;
        let transition = EmailTransition {
            tenant_id: tenant,
            resource_type: EmailResourceType::Payslip,
            resource_id: row.try_get("", "id")?,
            resource_version: email_timestamp_version("GENERATED", at),
            occurred_at: at,
            actor_ref: actor.clone(),
            actor_target_user_ids: vec![],
        };
        let reviewer =
            local_transition_reviewer(&transition, row.try_get("", "user_id")?, "generated");
        count += append_transition_emails(
            tx,
            &transition,
            EmailEventKind::PayslipAvailable,
            vec![reviewer],
        )
        .await?;
    }
    Ok(count)
}
