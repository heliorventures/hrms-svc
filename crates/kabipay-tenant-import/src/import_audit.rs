//! Persisted import facts are committed atomically with tenant domain changes.
use crate::{
    contract::ImportPackage,
    preview::{self, ImportPlan},
    report::ImportReport,
};
use anyhow::Result;
use kabipay_db_entities::tenant::d0092_tenant_import_tracking::{
    tenant_import_record, tenant_import_run,
};
use sea_orm::{ActiveModelTrait, DatabaseTransaction, Set};
use uuid::Uuid;
pub async fn persist(
    transaction: &DatabaseTransaction,
    report: &ImportReport,
    package: &ImportPackage,
    plan: &ImportPlan,
    actor: Uuid,
) -> Result<()> {
    let mut committed = report.clone();
    committed.committed = true;
    tenant_import_run::ActiveModel {
        id: Set(report.run_id),
        tenant_id: Set(report.tenant_id),
        package_hash: Set(plan.package_hash.clone()),
        configuration_hash: Set(plan.configuration_hash.clone()),
        target_hash: Set(plan.state_hash.clone()),
        mode: Set(plan.mode.clone()),
        actor_id: Set(actor),
        report: Set(serde_json::to_value(&committed)?),
        committed_at: Set(chrono::Utc::now()),
    }
    .insert(transaction)
    .await?;
    let mut seen = std::collections::HashSet::new();
    for item in &report.sections {
        let key = (
            item.source_ref.sheet.clone(),
            item.source_ref.row,
            item.section.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let row = package.employees.iter().find(|row| {
            row.source_ref.sheet == item.source_ref.sheet
                && row.source_ref.row == item.source_ref.row
        });
        let code = row
            .and_then(|r| r.employee.code.clone())
            .unwrap_or_else(|| {
                format!(
                    "UNRESOLVED_{}_{}",
                    &preview::hash(item.source_ref.sheet.as_bytes())[..16],
                    item.source_ref.row
                )
            });
        tenant_import_record::ActiveModel {
            tenant_id: Set(report.tenant_id),
            run_id: Set(report.run_id),
            employee_code: Set(code),
            section: Set(item.section.clone()),
            content_hash: Set(preview::hash(&serde_json::to_vec(&row)?)),
            outcome: Set(item.outcome.clone()),
            source_ref: Set(serde_json::to_value(&item.source_ref)?),
        }
        .insert(transaction)
        .await?;
    }
    Ok(())
}
