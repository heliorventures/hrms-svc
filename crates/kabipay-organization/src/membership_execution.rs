//! Desired membership configuration is installed and acknowledged by the bound source.
use crate::{
    execution::Execution, identity::SourceIdentity, runtime_policy::lifecycle_membership, store::*,
};
use kabipay_common::KabiPayResult;
use kabipay_organization_protocol::{
    MembershipInstallation, OrganizationCommand, OrganizationCommandOutcome, OrganizationOperation,
};
use kabipay_organization_security::command_body_hash;
use sea_orm::{DatabaseTransaction, TransactionTrait};
use uuid::Uuid;
pub(crate) async fn configuration(
    tx: &DatabaseTransaction,
    org: Uuid,
    member: Uuid,
) -> KabiPayResult<MembershipInstallation> {
    let (tenant, version) = lifecycle_membership(tx, org, member).await?;
    let r = one(
        tx,
        "SELECT status FROM kabipay_ops.organization_company WHERE id=$1 AND organization_id=$2",
        vec![member.into(), org.into()],
    )
    .await?;
    let status: String = r.try_get("", "status")?;
    let mut g = MembershipInstallation {
        organization_id: org,
        membership_id: member,
        tenant_id: tenant,
        status: if status == "DETACHED" {
            "DETACHING".into()
        } else {
            status
        },
        version,
        configuration_hash: String::new(),
    };
    g.configuration_hash = command_body_hash(&serde_json::to_vec(&g)?);
    Ok(g)
}
impl Execution {
    pub async fn install_membership(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
        expected: i64,
        command: Uuid,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        self.execute(
            actor,
            org,
            member,
            OrganizationCommand {
                command_id: command,
                resource_id: member,
                expected_version: expected,
                operation: OrganizationOperation::InstallMembership,
                payload: serde_json::json!({}),
            },
        )
        .await
    }
    pub(crate) async fn ensure_membership(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
    ) -> KabiPayResult<()> {
        let tx = self.store.db.begin().await?;
        let r=one(&tx,"SELECT version,installed_version,runtime_status FROM kabipay_ops.organization_company WHERE id=$1 AND organization_id=$2 AND status='ACTIVE'",vec![member.into(),org.into()]).await?;
        let v: i64 = r.try_get("", "version")?;
        let ready = r.try_get::<Option<i64>>("", "installed_version")? == Some(v)
            && r.try_get::<String>("", "runtime_status")? == "ACTIVE";
        tx.commit().await?;
        if !ready {
            let out = self
                .install_membership(actor, org, member, v, Uuid::new_v4())
                .await?;
            if out.status != "COMMITTED" {
                return Err(kabipay_common::KabiPayError::Conflict(
                    "company runtime activation awaits source confirmation".into(),
                ));
            }
        }
        Ok(())
    }
}
