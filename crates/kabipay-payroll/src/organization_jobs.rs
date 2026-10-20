//! Source-owned payroll jobs. Command receipts acknowledge acceptance only.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{OrganizationCommand, OrganizationOperation};
use kabipay_organization_tenant::{statement, unavailable, VerifiedRuntimeAuthorization};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftInput {
    pub expected_revision: Option<i32>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalizeInput {
    pub revision: i32,
    pub fingerprint: String,
    pub acknowledgement: crate::services::payroll_draft::FinalizeAcknowledgement,
}

pub(crate) fn validate_input(c: &OrganizationCommand) -> KabiPayResult<()> {
    match c.operation {
        OrganizationOperation::PayrollDraft => {
            let input: DraftInput = serde_json::from_value(c.payload.clone())?;
            if input.expected_revision.is_some_and(|r| r <= 0) {
                return Err(KabiPayError::Validation("Invalid draft revision".into()));
            }
        }
        OrganizationOperation::PayrollFinalize => {
            let input: FinalizeInput = serde_json::from_value(c.payload.clone())?;
            if input.revision <= 0
                || input.fingerprint.len() != 64
                || !input.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                || input.acknowledgement.provisional_tax_employees.len() > 10000
            {
                return Err(KabiPayError::Validation(
                    "Invalid payroll finalization input".into(),
                ));
            }
        }
        _ => return Err(unavailable()),
    }
    Ok(())
}

pub async fn enqueue(
    tx: &DatabaseTransaction,
    p: &VerifiedRuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    p.payroll_authority()?;
    validate_input(c)?;
    let verified_command = kabipay_organization_tenant::parse_command(p.request_body())?;
    if c.command_id != p.command_id()
        || serde_json::to_value(&verified_command)? != serde_json::to_value(c)?
    {
        return Err(unavailable());
    }
    // Transport already holds this cycle's row lock. Do not lock an existing
    // job here: the worker owns its job row before it locks the cycle.
    if tx.query_one(statement("SELECT id FROM organization_payroll_job WHERE tenant_id=$1 AND cycle_id=$2 AND state IN ('QUEUED','RUNNING')",vec![p.tenant_id.into(),c.resource_id.into()])).await?.is_some() {
        return Err(KabiPayError::Conflict("Payroll processing is already queued or running for this cycle; check its job status".into()));
    }
    let mut reference = (**p).clone();
    reference.grant = None;
    reference.relationship = None;
    reference.membership = None;
    reference.actor_target_user_ids.clear();
    let operation = serde_json::to_value(c.operation)?;
    let operation = operation.as_str().ok_or_else(unavailable)?;
    // The transport serializes acceptance + this insert in one source transaction.
    // The original command ID is the job ID; exact replays return its receipt.
    tx.execute(statement("INSERT INTO organization_payroll_job(tenant_id,id,cycle_id,principal_id,organization_id,membership_id,authority_id,authority_version,operation,request_body,request_hash,authority_reference,expected_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)", vec![p.tenant_id.into(),c.command_id.into(),c.resource_id.into(),p.principal_id.into(),p.organization_id.into(),p.membership_id.into(),p.authority_id.into(),p.authority_version.into(),operation.to_owned().into(),p.request_body().to_vec().into(),p.request_hash().to_owned().into(),serde_json::to_value(reference)?.into(),c.expected_version.into()])).await?;
    Ok(
        serde_json::json!({"job_id":c.command_id,"cycle_id":c.resource_id,"state":"QUEUED","accepted":true,"version":1}),
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PageInput {
    after: Option<Uuid>,
    #[serde(default = "page_size")]
    first: u32,
}
fn page_size() -> u32 {
    50
}

const FIELDS: &str = "id,cycle_id,operation,state,version,attempts,result,failure_code,created_at,started_at,finished_at";
fn public_job(row: &sea_orm::QueryResult) -> KabiPayResult<serde_json::Value> {
    Ok(serde_json::json!({
        "job_id":row.try_get::<Uuid>("","id")?,
        "cycle_id":row.try_get::<Uuid>("","cycle_id")?,
        "operation":row.try_get::<String>("","operation")?,
        "state":row.try_get::<String>("","state")?,
        "version":row.try_get::<i64>("","version")?,
        "attempts":row.try_get::<i32>("","attempts")?,
        "result":row.try_get::<Option<serde_json::Value>>("","result")?,
        "failure_code":row.try_get::<Option<String>>("","failure_code")?,
        "created_at":row.try_get::<chrono::DateTime<chrono::Utc>>("","created_at")?,
        "started_at":row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("","started_at")?,
        "finished_at":row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("","finished_at")?
    }))
}

pub async fn read_or_cancel(
    tx: &DatabaseTransaction,
    p: &VerifiedRuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    let identity = serde_json::json!({
        "source_tenant_id":p.source_tenant_id,"source_user_id":p.source_user_id,
        "identity_version":p.identity_version,"source_link_version":p.source_link_version,
        "membership_version":p.membership_version
    });
    if c.operation == OrganizationOperation::PayrollJobList {
        if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
            return Err(unavailable());
        }
        let input: PageInput = serde_json::from_value(c.payload.clone())?;
        if input.first == 0 || input.first > 100 {
            return Err(KabiPayError::Validation(
                "Payroll job page size must be between 1 and 100".into(),
            ));
        }
        let rows = tx.query_all(statement(&format!("SELECT {FIELDS} FROM organization_payroll_job WHERE tenant_id=$1 AND organization_id=$2 AND membership_id=$3 AND principal_id=$4 AND authority_id=$5 AND authority_version=$6 AND authority_reference @> $9::jsonb AND ($7::uuid IS NULL OR id>$7) ORDER BY id LIMIT $8"),vec![p.tenant_id.into(),p.organization_id.into(),p.membership_id.into(),p.principal_id.into(),p.authority_id.into(),p.authority_version.into(),input.after.into(),i64::from(input.first+1).into(),identity.into()])).await?;
        let has_more = rows.len() > input.first as usize;
        let jobs = rows
            .iter()
            .take(input.first as usize)
            .map(public_job)
            .collect::<KabiPayResult<Vec<_>>>()?;
        return Ok(
            serde_json::json!({"after":jobs.last().and_then(|j|j.get("job_id")),"jobs":jobs,"has_more":has_more}),
        );
    }
    let _: kabipay_organization_tenant::EmptyPayload = serde_json::from_value(c.payload.clone())?;
    let row = tx.query_one(statement(&format!("SELECT {FIELDS} FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2 AND organization_id=$3 AND membership_id=$4 AND principal_id=$5 AND authority_id=$6 AND authority_version=$7 AND authority_reference @> $8::jsonb FOR UPDATE"),vec![p.tenant_id.into(),c.resource_id.into(),p.organization_id.into(),p.membership_id.into(),p.principal_id.into(),p.authority_id.into(),p.authority_version.into(),identity.into()])).await?.ok_or_else(unavailable)?;
    if row.try_get::<i64>("", "version")? != c.expected_version {
        return Err(KabiPayError::Conflict(
            "Payroll job changed; reload before continuing".into(),
        ));
    }
    if c.operation == OrganizationOperation::PayrollJobCancelBeforeExecution {
        if row.try_get::<String>("", "state")? != "QUEUED" {
            return Err(KabiPayError::Conflict(
                "Payroll job has already started or finished".into(),
            ));
        }
        let updated = tx.query_one(statement(&format!("UPDATE organization_payroll_job SET state='CANCELLED',failure_code=NULL,finished_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 AND state='QUEUED' RETURNING {FIELDS}"),vec![p.tenant_id.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
        return public_job(&updated);
    }
    if c.operation != OrganizationOperation::PayrollJobDetail {
        return Err(unavailable());
    }
    public_job(&row)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acceptance_checks_bounded_typed_input_without_business_execution() {
        let mut command = OrganizationCommand {
            command_id: Uuid::new_v4(),
            resource_id: Uuid::new_v4(),
            expected_version: 1,
            operation: OrganizationOperation::PayrollFinalize,
            payload: serde_json::json!({"revision":1,"fingerprint":"a".repeat(64),"acknowledgement":{"provisional_tax_employees":[]}}),
        };
        assert!(validate_input(&command).is_ok());
        command.payload["fingerprint"] = serde_json::json!("unbound");
        assert!(validate_input(&command).is_err());
        command.operation = OrganizationOperation::PayrollDraft;
        command.payload =
            serde_json::json!({"expected_revision":null,"delegation_token":"forbidden"});
        assert!(validate_input(&command).is_err());
    }
}
