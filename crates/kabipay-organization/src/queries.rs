use crate::{identity::SourceIdentity, policy::MANAGE, store::*};
use async_graphql::{Enum, Json, SimpleObject};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, TransactionTrait};
use uuid::Uuid;
#[derive(Clone, Copy, PartialEq, Eq, Enum)]
pub enum OrganizationConfigurationKind {
    Companies,
    Roles,
    Bindings,
}
#[derive(SimpleObject)]
pub struct OrganizationConfigurationPage {
    pub schema_version: i32,
    pub items: Json<Vec<serde_json::Value>>,
    pub next_cursor: Option<Uuid>,
    pub organization_version: i64,
}
#[derive(SimpleObject)]
pub struct OrganizationIdentity {
    pub principal_id: Uuid,
    pub login_link_id: Uuid,
    pub login_link_version: i64,
}
impl Store {
    pub async fn identity(&self, actor: &SourceIdentity) -> KabiPayResult<OrganizationIdentity> {
        let tx = self.db.begin().await?;
        let principal = current(&tx, actor).await?;
        tx.commit().await?;
        Ok(OrganizationIdentity {
            principal_id: principal.id,
            login_link_id: principal.link,
            login_link_version: principal.link_version,
        })
    }
    pub async fn configuration(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        kind: OrganizationConfigurationKind,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationConfigurationPage> {
        valid_id(org)?;
        if !(1..=100).contains(&limit) {
            return Err(KabiPayError::Validation(
                "page size must be between 1 and 100".into(),
            ));
        }
        if let Some(after) = after {
            valid_id(after)?;
        }
        let tx = self.db.begin().await?;
        let organization=one(&tx,"SELECT version FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR SHARE",vec![org.into()]).await?;
        let principal = current(&tx, actor).await?;
        if authorized(&tx, actor, principal.id, org, MANAGE)
            .await
            .is_err()
        {
            authorized(&tx, actor, principal.id, org, crate::policy::DELEGATE).await?;
        }
        let query=match kind {
            OrganizationConfigurationKind::Companies=>"SELECT m.id,to_jsonb(m) AS data FROM kabipay_ops.organization_company m WHERE m.organization_id=$1 AND ($2::uuid IS NULL OR m.id>$2) ORDER BY m.id LIMIT $3",
            OrganizationConfigurationKind::Roles=>"SELECT r.id,to_jsonb(r)||jsonb_build_object('capabilities',COALESCE((SELECT jsonb_agg(to_jsonb(c) ORDER BY c.capability_code) FROM kabipay_ops.organization_role_capability c WHERE c.organization_id=r.organization_id AND c.role_id=r.id),'[]'::jsonb)) AS data FROM kabipay_ops.organization_role r WHERE r.organization_id=$1 AND ($2::uuid IS NULL OR r.id>$2) ORDER BY r.id LIMIT $3",
            OrganizationConfigurationKind::Bindings=>"SELECT b.id,to_jsonb(b)||jsonb_build_object('company_scopes',COALESCE((SELECT jsonb_agg(to_jsonb(s) ORDER BY s.organization_company_id) FROM kabipay_ops.role_binding_company s WHERE s.organization_id=b.organization_id AND s.binding_id=b.id),'[]'::jsonb)) AS data FROM kabipay_ops.principal_role_binding b WHERE b.organization_id=$1 AND ($2::uuid IS NULL OR b.id>$2) ORDER BY b.id LIMIT $3",
        };
        let mut rows = tx
            .query_all(sql(
                query,
                vec![org.into(), after.into(), i64::from(limit + 1).into()],
            ))
            .await?;
        let more = rows.len() > limit as usize;
        if more {
            rows.pop();
        }
        let next_cursor = if more {
            rows.last().map(|row| row.try_get("", "id")).transpose()?
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(|row| row.try_get("", "data"))
            .collect::<Result<Vec<serde_json::Value>, _>>()?;
        let version = organization.try_get("", "version")?;
        tx.commit().await?;
        Ok(OrganizationConfigurationPage {
            schema_version: 1,
            items: Json(items),
            next_cursor,
            organization_version: version,
        })
    }
}
