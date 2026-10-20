use crate::{
    identity::SourceIdentity,
    models::Organization,
    store::{current, denied, event, one, sql, valid_name, EventSubject, Store},
};
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait, TransactionTrait};
use uuid::Uuid;
impl Store {
    /// Operator-only process entry point; never register as a public resolver.
    pub async fn bootstrap(
        &self,
        actor: &SourceIdentity,
        name: &str,
    ) -> KabiPayResult<Organization> {
        valid_name(name, 255)?;
        if !actor.may_accept_company() {
            return Err(denied());
        }
        let tx = self.db.begin().await?;
        one(&tx,"SELECT id FROM kabipay_ops.tenant WHERE id=$1 AND status='ACTIVE' AND NOT is_deleted FOR UPDATE",vec![actor.tenant().into()]).await?;
        let occupied=tx.query_one(sql("SELECT id FROM kabipay_ops.organization_company WHERE tenant_id=$1 AND status IN ('ACTIVE','SUSPENDED','DETACHING')",vec![actor.tenant().into()])).await?;
        if occupied.is_some() {
            return Err(denied());
        }
        Self::register_in(&tx, actor).await?;
        let principal = current(&tx, actor).await?;
        let id = Uuid::new_v4();
        let membership = Uuid::new_v4();
        let role = Uuid::new_v4();
        let binding = Uuid::new_v4();
        tx.execute(sql(
            "INSERT INTO kabipay_ops.organization(id,name,status) VALUES($1,$2,'ACTIVE')",
            vec![id.into(), name.into()],
        ))
        .await?;
        tx.execute(sql("INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id,status,effective_from) VALUES($1,$2,$3,'ACTIVE',NOW())",vec![membership.into(),id.into(),actor.tenant().into()])).await?;
        tx.execute(sql("INSERT INTO kabipay_ops.organization_role(id,organization_id,name,status) VALUES($1,$2,'Organization administrator','ACTIVE')",vec![role.into(),id.into()])).await?;
        for code in [crate::policy::MANAGE, crate::policy::DELEGATE] {
            tx.execute(sql("INSERT INTO kabipay_ops.organization_role_capability(organization_id,role_id,capability_code,employee_scope) VALUES($1,$2,$3,'ALL')",vec![id.into(),role.into(),code.into()])).await?;
        }
        tx.execute(sql("INSERT INTO kabipay_ops.principal_role_binding(id,organization_id,principal_id,role_id,status,effective_from,granted_by) VALUES($1,$2,$3,$4,'ACTIVE',NOW(),$3)",vec![binding.into(),id.into(),principal.id.into(),role.into()])).await?;
        tx.execute(sql("INSERT INTO kabipay_ops.role_binding_company(organization_id,binding_id,organization_company_id,activation_status) VALUES($1,$2,$3,'ACTIVE')",vec![id.into(),binding.into(),membership.into()])).await?;
        event(&tx,actor,principal.id,EventSubject{organization:Some(id),id,version:1},"organization.bootstrapped",serde_json::json!({"organization_id":id,"name":name,"membership_id":membership,"role_id":role,"binding_id":binding,"capabilities":[crate::policy::MANAGE,crate::policy::DELEGATE],"status":"ACTIVE"})).await?;
        tx.commit().await?;
        Ok(Organization {
            id,
            name: name.into(),
            status: "ACTIVE".into(),
            version: 1,
        })
    }
}
