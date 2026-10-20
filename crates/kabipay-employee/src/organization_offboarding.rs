//! HR exit initiation and recovery; Finance authorization and FNF remain independently owned.
use crate::services::{finance_operation, offboarding_fnf_service, separation_service};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{OrganizationCommand, OrganizationOperation as Op};
use kabipay_organization_tenant::{statement, unavailable, VerifiedRuntimeAuthorization};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SeparationCreateInput {
    employee_id: Uuid,
    separation_type: String,
    resignation_date: Option<chrono::NaiveDate>,
    last_working_date: chrono::NaiveDate,
    reason: Option<String>,
}

pub(super) async fn execute(
    db: &DatabaseTransaction,
    p: &VerifiedRuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    let tenant = p.tenant_id;
    if c.operation == Op::SeparationList {
        if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
            return Err(unavailable());
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Page {
            after: Option<Uuid>,
            first: u32,
        }
        let i: Page = serde_json::from_value(c.payload.clone())?;
        if i.first == 0 || i.first > 100 {
            return Err(KabiPayError::Validation(
                "Separation page size must be between 1 and 100".into(),
            ));
        }
        let rows=db.query_all(statement("SELECT s.id,s.employee_id,concat_ws(' ',e.first_name,e.last_name) AS employee_name,e.employee_code,s.separation_type,s.last_working_date,s.status,v.version FROM separation s JOIN employee e ON e.id=s.employee_id AND e.tenant_id=s.tenant_id JOIN organization_resource_version v ON v.tenant_id=s.tenant_id AND v.resource_type='separation' AND v.resource_id=s.id WHERE s.tenant_id=$1 AND ($2::uuid IS NULL OR s.id>$2) ORDER BY s.id LIMIT $3",vec![tenant.into(),i.after.into(),i64::from(i.first).into()])).await?;
        let mut separations = Vec::new();
        for row in rows {
            separations.push(serde_json::json!({"separation_id":row.try_get::<Uuid>("","id")?,"employee_id":row.try_get::<Uuid>("","employee_id")?,"employee_name":row.try_get::<String>("","employee_name")?,"employee_code":row.try_get::<String>("","employee_code")?,"separation_type":row.try_get::<String>("","separation_type")?,"last_working_date":row.try_get::<chrono::NaiveDate>("","last_working_date")?,"status":row.try_get::<String>("","status")?,"version":row.try_get::<i64>("","version")?}));
        }
        return Ok(
            serde_json::json!({"after":separations.last().and_then(|s|s.get("separation_id")),"has_more":separations.len()==i.first as usize,"separations":separations}),
        );
    }
    match c.operation {
        Op::SeparationCreate => {
            let i: SeparationCreateInput = serde_json::from_value(c.payload.clone())?;
            separation_service::insert_separation_in_transaction(
                db,
                tenant,
                c.resource_id,
                i.employee_id,
                i.separation_type,
                i.resignation_date,
                i.last_working_date,
                i.reason,
            )
            .await?;
        }
        Op::SeparationReject => {
            let _: kabipay_organization_tenant::EmptyPayload =
                serde_json::from_value(c.payload.clone())?;
            separation_service::reject_with_principal_in_transaction(
                db,
                tenant,
                c.resource_id,
                p.principal_id,
            )
            .await?;
        }
        Op::SeparationCancelApproval => {
            let _: kabipay_organization_tenant::EmptyPayload =
                serde_json::from_value(c.payload.clone())?;
            separation_service::cancel_approval_in_transaction(db, tenant, c.resource_id).await?;
        }
        Op::ClearanceUpdate => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                is_cleared: bool,
            }
            let i: Input = serde_json::from_value(c.payload.clone())?;
            let r = offboarding_fnf_service::set_clearance_in_transaction(
                db,
                tenant,
                c.resource_id,
                i.is_cleared,
                None,
                Some(p.principal_id),
            )
            .await?;
            return Ok(
                serde_json::json!({"clearance_id":r.id,"separation_id":r.separation_id,"is_cleared":r.is_cleared,"cleared_at":r.cleared_at,"cleared_by_principal_id":if r.is_cleared{Some(p.principal_id)}else{None}}),
            );
        }
        Op::SeparationDetail => {
            let _: kabipay_organization_tenant::EmptyPayload =
                serde_json::from_value(c.payload.clone())?;
        }
        _ => return Err(unavailable()),
    }
    let row=db.query_one(statement("SELECT s.id,s.employee_id,concat_ws(' ',e.first_name,e.last_name) AS employee_name,e.employee_code,s.separation_type,s.resignation_date,s.last_working_date,s.reason,s.status,s.approved_by,s.approved_by_principal_id,s.offboarded_at FROM separation s JOIN employee e ON e.id=s.employee_id AND e.tenant_id=s.tenant_id WHERE s.tenant_id=$1 AND s.id=$2",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
    let clearance=db.query_all(statement("SELECT c.id,c.department,c.task_name,c.is_cleared,c.cleared_at,c.cleared_by,c.cleared_by_principal_id,v.version FROM clearance_checklist c JOIN organization_resource_version v ON v.tenant_id=c.tenant_id AND v.resource_type='clearance_checklist' AND v.resource_id=c.id WHERE c.tenant_id=$1 AND c.separation_id=$2 ORDER BY c.id LIMIT 200",vec![tenant.into(),c.resource_id.into()])).await?;
    let mut items = Vec::new();
    for r in clearance {
        items.push(serde_json::json!({"clearance_id":r.try_get::<Uuid>("","id")?,"department":r.try_get::<String>("","department")?,"task_name":r.try_get::<String>("","task_name")?,"is_cleared":r.try_get::<bool>("","is_cleared")?,"cleared_at":r.try_get::<Option<chrono::DateTime<chrono::Utc>>>("","cleared_at")?,"cleared_by":r.try_get::<Option<Uuid>>("","cleared_by")?,"cleared_by_principal_id":r.try_get::<Option<Uuid>>("","cleared_by_principal_id")?,"version":r.try_get::<i64>("","version")?}));
    }
    let pending = finance_operation::latest_for_separation(db, tenant, c.resource_id)
        .await?
        .map(|o| serde_json::json!({"operation_id":o.id,"state":o.state}));
    Ok(
        serde_json::json!({"separation_id":row.try_get::<Uuid>("","id")?,"employee_id":row.try_get::<Uuid>("","employee_id")?,"employee_name":row.try_get::<String>("","employee_name")?,"employee_code":row.try_get::<String>("","employee_code")?,"separation_type":row.try_get::<String>("","separation_type")?,"resignation_date":row.try_get::<Option<chrono::NaiveDate>>("","resignation_date")?,"last_working_date":row.try_get::<chrono::NaiveDate>("","last_working_date")?,"reason":row.try_get::<Option<String>>("","reason")?,"status":row.try_get::<String>("","status")?,"approved_by":row.try_get::<Option<Uuid>>("","approved_by")?,"approved_by_principal_id":row.try_get::<Option<Uuid>>("","approved_by_principal_id")?,"offboarded_at":row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("","offboarded_at")?,"clearance":items,"approval_operation":pending}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_request_cannot_include_finance_authority_or_actor_override() {
        let base = serde_json::json!({"employee_id":Uuid::new_v4(),"separation_type":"RESIGNATION","last_working_date":"2026-10-31"});
        for key in [
            "approved_by",
            "principal_id",
            "finance_authorization_id",
            "net_payable",
        ] {
            let mut value = base.clone();
            value[key] = serde_json::json!("forged");
            assert!(serde_json::from_value::<SeparationCreateInput>(value).is_err());
        }
    }
}
