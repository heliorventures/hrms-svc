use crate::services::payroll_draft;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::*;
use kabipay_organization_tenant::{
    statement, unavailable, SourceAdapter, VerifiedRuntimeAuthorization,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;
#[derive(Clone)]
pub struct PayrollAdapter;
fn default_first() -> u32 {
    50
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    cycle_id: Option<Uuid>,
    after: Option<Uuid>,
    #[serde(default = "default_first")]
    first: u32,
}
impl SourceAdapter for PayrollAdapter {
    fn service(&self) -> &'static str {
        "kabipay-payroll"
    }
    async fn employee(
        &self,
        _: &DatabaseConnection,
        _: Uuid,
        _: &OrganizationCommand,
    ) -> KabiPayResult<Option<Uuid>> {
        Ok(None)
    }
    async fn execute(
        &self,
        tx: &DatabaseTransaction,
        _: &DatabaseConnection,
        p: &VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        if p.employee_scope != "ALL" || p.authority_kind != AuthorityKind::OrganizationGrant {
            return Err(unavailable());
        }
        match c.operation {
            OrganizationOperation::PayrollList | OrganizationOperation::PayrollDetail => {
                let input: ListInput = serde_json::from_value(c.payload.clone())?;
                if input.first == 0 || input.first > 100 {
                    return Err(KabiPayError::Validation(
                        "Payroll page size must be between 1 and 100".into(),
                    ));
                }
                if c.operation == OrganizationOperation::PayrollDetail {
                    let id = input.cycle_id.ok_or_else(unavailable)?;
                    if id != c.resource_id {
                        return Err(unavailable());
                    }
                    let cycle = payroll_draft::cycle(tx, p.tenant_id, id).await?;
                    let draft = payroll_draft::find_with_date(
                        tx,
                        p.tenant_id,
                        id,
                        p.payroll_business_date()?,
                    )
                    .await?;
                    return Ok(
                        serde_json::json!({"cycle":{"id":cycle.id,"name":cycle.name,"year":cycle.year,"month":cycle.month,"status":cycle.status,"payment_date":cycle.payment_date},"draft":draft}),
                    );
                }
                if input.cycle_id.is_some() {
                    return Err(KabiPayError::Validation(
                        "use PayrollDetail for a source cycle".into(),
                    ));
                }
                if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
                    return Err(unavailable());
                }
                let rows=tx.query_all(statement("SELECT c.id,c.name,c.year,c.month,c.status,v.version,d.revision,d.fingerprint,d.snapshot->>'can_finalize' AS can_finalize,d.snapshot->>'finalization_block_reason' AS finalization_block_reason FROM payroll_cycle c JOIN organization_resource_version v ON v.tenant_id=c.tenant_id AND v.resource_id=c.id AND v.resource_type='payroll_run' LEFT JOIN payroll_draft_calculation d ON d.tenant_id=c.tenant_id AND d.cycle_id=c.id WHERE c.tenant_id=$1 AND ($2::uuid IS NULL OR c.id>$2) ORDER BY c.id LIMIT $3",vec![p.tenant_id.into(),input.after.into(),i64::from(input.first).into()])).await?;
                let mut cycles = Vec::new();
                for row in rows {
                    cycles.push(serde_json::json!({"id":row.try_get::<Uuid>("","id")?,"name":row.try_get::<String>("","name")?,"year":row.try_get::<i32>("","year")?,"month":row.try_get::<i32>("","month")?,"status":row.try_get::<String>("","status")?,"version":row.try_get::<i64>("","version")?,"draft_revision":row.try_get::<Option<i32>>("","revision")?,"fingerprint":row.try_get::<Option<String>>("","fingerprint")?,"can_finalize":row.try_get::<Option<String>>("","can_finalize")?.map(|s|s=="true"),"finalization_block_reason":row.try_get::<Option<String>>("","finalization_block_reason")?}));
                }
                let after = cycles.last().and_then(|v| v.get("id")).cloned();
                let has_more = cycles.len() == input.first as usize;
                Ok(serde_json::json!({"cycles":cycles,"after":after,"has_more":has_more}))
            }
            OrganizationOperation::PayrollDraft | OrganizationOperation::PayrollFinalize => {
                crate::organization_jobs::enqueue(tx, p, c).await
            }
            OrganizationOperation::PayrollJobList
            | OrganizationOperation::PayrollJobDetail
            | OrganizationOperation::PayrollJobCancelBeforeExecution => {
                crate::organization_jobs::read_or_cancel(tx, p, c).await
            }
            _ => Err(unavailable()),
        }
    }
}
