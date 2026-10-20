use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::*;
use kabipay_organization_tenant::{statement, unavailable, SourceAdapter};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct EmployeeAdapter;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryInput {
    after: Option<Uuid>,
    first: u32,
}
impl SourceAdapter for EmployeeAdapter {
    fn service(&self) -> &'static str {
        "kabipay-employee"
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
        db: &DatabaseTransaction,
        _: &DatabaseConnection,
        p: &kabipay_organization_tenant::VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        if c.operation != OrganizationOperation::Directory {
            return crate::organization_admin::execute(db,p,c).await;
        }
        if c.resource_id != p.membership_id || c.expected_version != p.membership_version {
            return Err(unavailable());
        }
        kabipay_organization_tenant::queue_bootstrap(db, p.tenant_id, p.organization_id).await?;
        let input: DirectoryInput = serde_json::from_value(c.payload.clone())?;
        if input.first == 0 || input.first > 200 {
            return Err(KabiPayError::Validation(
                "Directory page size must be between 1 and 200".into(),
            ));
        }
        let rows=db.query_all(statement("SELECT e.id,e.user_id,u.id AS linked_user_id,(SELECT count(*) FROM employee linked WHERE linked.tenant_id=e.tenant_id AND linked.user_id=e.user_id AND NOT linked.is_deleted) AS link_count,e.is_deleted,e.first_name,e.last_name,e.employee_code,e.status,e.department_id,e.designation_id,e.reporting_manager_id,v.version FROM employee e LEFT JOIN \"user\" u ON u.tenant_id=e.tenant_id AND u.id=e.user_id JOIN organization_resource_version v ON v.tenant_id=e.tenant_id AND v.resource_type='employee' AND v.resource_id=e.id WHERE e.tenant_id=$1 AND ($2::uuid IS NULL OR e.id>$2) ORDER BY e.id LIMIT $3",vec![p.tenant_id.into(),input.after.into(),i64::from(input.first).into()])).await?;
        let mut employees = Vec::new();
        for row in rows {
            let id: Uuid = row.try_get("", "id")?;
            let user: Option<Uuid> = row.try_get("", "user_id")?;
            let deleted: bool = row.try_get("", "is_deleted")?;
            let links: i64 = row.try_get("", "link_count")?;
            if user.is_some()
                && (row.try_get::<Option<Uuid>>("", "linked_user_id")? != user
                    || links > 1
                    || (!deleted && links != 1))
            {
                return Err(KabiPayError::Conflict(
                    "Employee directory contains an ambiguous or missing source account link"
                        .into(),
                ));
            }
            let status = if deleted {
                "DELETED".to_owned()
            } else {
                row.try_get::<String>("", "status")?
            };
            employees.push(serde_json::json!({"employee_id":id,"user_id":user,"display_name":format!("{} {}",row.try_get::<String>("","first_name")?,row.try_get::<String>("","last_name")?),"employee_code":row.try_get::<String>("","employee_code")?,"status":status,"is_deleted":deleted,"department_id":row.try_get::<Option<Uuid>>("","department_id")?,"designation_id":row.try_get::<Option<Uuid>>("","designation_id")?,"reporting_manager_id":row.try_get::<Option<Uuid>>("","reporting_manager_id")?,"version":row.try_get::<i64>("","version")?}));
        }
        let after = employees.last().and_then(|v| v.get("employee_id")).cloned();
        let bootstrap=db.query_one(statement("SELECT status,phase FROM organization_fact_bootstrap WHERE tenant_id=$1 AND organization_id=$2",vec![p.tenant_id.into(),p.organization_id.into()])).await?.ok_or_else(unavailable)?;
        Ok(
            serde_json::json!({"employees":employees,"after":after,"has_more":employees.len()==input.first as usize,"source_bootstrap":{"status":bootstrap.try_get::<String>("","status")?,"phase":bootstrap.try_get::<i32>("","phase")?}}),
        )
    }
}
