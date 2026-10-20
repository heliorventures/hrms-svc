use crate::{
    identity::SourceIdentity,
    models::{Change, ChangeResult},
    policy::{DELEGATE, MANAGE},
    store::*,
};
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait, TransactionTrait};
use uuid::Uuid;
impl Store {
    pub async fn apply(
        &self,
        actor: &SourceIdentity,
        organization: Uuid,
        version: i64,
        change: Change,
    ) -> KabiPayResult<ChangeResult> {
        valid_id(organization)?;
        if version <= 0 || version == i64::MAX {
            return Err(conflict());
        }
        let tx = self.db.begin().await?;
        let org=one(&tx,"SELECT version FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",vec![organization.into()]).await?;
        let principal = current(&tx, actor).await?;
        if org.try_get::<i64>("", "version")? != version {
            return Err(conflict());
        }
        if !matches!(
            &change,
            Change::AcceptCompany { .. } | Change::GrantRole { .. } | Change::RevokeBinding { .. }
        ) {
            authorized(&tx, actor, principal.id, organization, MANAGE).await?;
        }
        if matches!(
            &change,
            Change::CreateRole { .. }
                | Change::ReplaceRole { .. }
                | Change::GrantRole { .. }
                | Change::RevokeBinding { .. }
        ) {
            authorized(&tx, actor, principal.id, organization, DELEGATE).await?;
        }
        let record = serde_json::to_value(&change)?;
        let (id, status, new_version) = match change {
            Change::InviteCompany { tenant } => {
                crate::membership::invite(&tx, organization, tenant).await?
            }
            Change::AcceptCompany {
                membership,
                version,
            } => crate::membership::accept(&tx, actor, organization, membership, version).await?,
            Change::SuspendCompany {
                membership,
                version,
            } => crate::membership::suspend(&tx, actor, organization, membership, version).await?,
            Change::ResumeCompany {
                membership,
                version,
            } => crate::membership::resume(&tx, actor, organization, membership, version).await?,
            Change::BeginDetachCompany {
                membership,
                version,
            } => {
                crate::membership::begin_detach(&tx, actor, organization, membership, version)
                    .await?
            }
            Change::CreateRole { name, capabilities } => {
                crate::roles::create(&tx, organization, &name, &capabilities).await?
            }
            Change::ReplaceRole {
                role,
                version,
                capabilities,
            } => {
                crate::roles::replace(
                    &tx,
                    organization,
                    principal.id,
                    role,
                    version,
                    &capabilities,
                )
                .await?
            }
            Change::GrantRole {
                principal: subject,
                role,
                companies,
            } => {
                crate::roles::grant(&tx, organization, principal.id, subject, role, &companies)
                    .await?
            }
            Change::RevokeBinding { binding, version } => {
                crate::roles::revoke(&tx, organization, principal.id, binding, version).await?
            }
        };
        tx.execute(sql(
            "UPDATE kabipay_ops.organization SET version=version+1,updated_at=NOW() WHERE id=$1",
            vec![organization.into()],
        ))
        .await?;
        event(&tx,actor,principal.id,EventSubject{organization:Some(organization),id,version:new_version},"organization.configuration_changed",serde_json::json!({"request":record,"id":id,"status":status,"version":new_version,"organization_version":version+1})).await?;
        tx.commit().await?;
        Ok(ChangeResult {
            id,
            status: status.into(),
            version: new_version,
            organization_version: version + 1,
        })
    }
}
