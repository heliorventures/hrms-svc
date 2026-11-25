//! Owned Tax transitions only; no declaration edits or derived totals emit.
use kabipay_common::{context::ScopeType, important_email::*, KabiPayResult};
use kabipay_db_entities::tenant::d0031_tax_proof::tax_proof_line;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;
pub async fn emit_tax_email(
    tx: &DatabaseTransaction,
    row: &tax_proof_line::Model,
    actor: Uuid,
) -> KabiPayResult<usize> {
    let transition = EmailTransition {
        tenant_id: row.tenant_id,
        resource_type: EmailResourceType::TaxProof,
        resource_id: row.id,
        resource_version: email_timestamp_version(&row.status, row.updated_at),
        occurred_at: row.updated_at,
        actor_ref: Some(EmailActorRef::LocalUser { user_id: actor }),
        actor_target_user_ids: vec![],
    };
    if row.status == "PENDING" {
        append_local_review_emails(
            tx,
            &transition,
            row.employee_id,
            &["tax:approve"],
            &[ScopeType::All, ScopeType::Team],
            "pending",
        )
        .await
    } else if matches!(row.status.as_str(), "APPROVED" | "REJECTED") {
        kabipay_organization_tenant::email_emission::emit_requester_outcome(
            tx,
            transition,
            row.employee_id,
            row.status.to_ascii_lowercase(),
        )
        .await
    } else {
        Ok(0)
    }
}
