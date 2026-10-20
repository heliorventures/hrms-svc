use crate::{identity::SourceIdentity, models::Organization, policy::MANAGE};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;
#[derive(Clone)]
pub struct Store {
    pub(crate) db: DatabaseConnection,
}
pub(crate) struct Principal {
    pub id: Uuid,
    pub link: Uuid,
    pub link_version: i64,
    pub identity_version: i64,
    pub name: String,
}
pub(crate) struct EventSubject {
    pub organization: Option<Uuid>,
    pub id: Uuid,
    pub version: i64,
}
pub(crate) fn sql(query: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}
pub(crate) fn denied() -> KabiPayError {
    KabiPayError::Forbidden("organization authority unavailable".into())
}
pub(crate) fn conflict() -> KabiPayError {
    KabiPayError::Conflict("configuration changed; reload before retrying".into())
}
pub(crate) fn valid_id(id: Uuid) -> KabiPayResult<()> {
    if id.is_nil() {
        Err(KabiPayError::Validation("non-nil ID is required".into()))
    } else {
        Ok(())
    }
}
pub(crate) fn valid_name(name: &str, max: usize) -> KabiPayResult<()> {
    if name.trim() != name || name.is_empty() || name.len() > max {
        Err(KabiPayError::Validation(
            "name is required, trimmed and bounded".into(),
        ))
    } else {
        Ok(())
    }
}
pub(crate) async fn one<C: ConnectionTrait>(
    db: &C,
    query: &str,
    values: Vec<Value>,
) -> KabiPayResult<QueryResult> {
    db.query_one(sql(query, values)).await?.ok_or_else(denied)
}
pub(crate) async fn current(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
) -> KabiPayResult<Principal> {
    let row=one(tx,"SELECT p.id,p.identity_version,p.display_name,l.id AS link,l.version FROM kabipay_ops.principal p JOIN kabipay_ops.principal_login_link l ON l.principal_id=p.id JOIN kabipay_ops.tenant t ON t.id=l.source_tenant_id WHERE l.source_tenant_id=$1 AND l.source_user_id=$2 AND l.issuer='kabipay-client' AND l.status='VERIFIED' AND p.status='ACTIVE' AND t.status='ACTIVE' AND NOT t.is_deleted FOR SHARE OF p,l",vec![actor.tenant().into(),actor.user().into()]).await?;
    Ok(Principal {
        id: row.try_get("", "id")?,
        link: row.try_get("", "link")?,
        link_version: row.try_get("", "version")?,
        identity_version: row.try_get("", "identity_version")?,
        name: row.try_get("", "display_name")?,
    })
}
pub(crate) async fn authorized(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    principal: Uuid,
    org: Uuid,
    capability: &str,
) -> KabiPayResult<()> {
    one(tx,"SELECT b.id FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization o ON o.id=b.organization_id JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=o.id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=o.id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=o.id JOIN kabipay_ops.organization_company target ON target.id=s.organization_company_id AND target.organization_id=o.id JOIN kabipay_ops.tenant target_tenant ON target_tenant.id=target.tenant_id WHERE o.id=$1 AND o.status='ACTIVE' AND b.principal_id=$2 AND b.status='ACTIVE' AND r.status='ACTIVE' AND c.capability_code=$3 AND c.employee_scope='ALL' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND target_tenant.status='ACTIVE' AND NOT target_tenant.is_deleted AND target.status='ACTIVE' AND target.effective_from<=NOW() AND (target.effective_to IS NULL OR NOW()<target.effective_to) AND EXISTS(SELECT 1 FROM kabipay_ops.organization_company source WHERE source.organization_id=o.id AND source.tenant_id=$4 AND source.status='ACTIVE' AND source.effective_from<=NOW() AND (source.effective_to IS NULL OR NOW()<source.effective_to)) LIMIT 1",vec![org.into(),principal.into(),capability.into(),actor.tenant().into()]).await?;
    Ok(())
}
pub(crate) async fn event(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    principal: Uuid,
    subject: EventSubject,
    kind: &str,
    payload: serde_json::Value,
) -> KabiPayResult<()> {
    let payload = serde_json::json!({"actor":{"principal_id":principal,"source_tenant_id":actor.tenant(),"source_user_id":actor.user(),"issuer":"kabipay-client"},"configuration_version":subject.version,"change":payload});
    let event_version:i64=one(tx,"INSERT INTO kabipay_ops.organization_outbox_sequence(aggregate_id,version) VALUES($1,1) ON CONFLICT(aggregate_id) DO UPDATE SET version=organization_outbox_sequence.version+1 RETURNING version",vec![subject.id.into()]).await?.try_get("","version")?;
    tx.execute(sql("INSERT INTO kabipay_ops.organization_outbox(id,organization_id,source_tenant_id,actor_principal_id,aggregate_id,aggregate_version,event_type,schema_version,payload) VALUES($1,$2,$3,$4,$5,$6,$7,1,$8)",vec![Uuid::new_v4().into(),subject.organization.into(),actor.tenant().into(),principal.into(),subject.id.into(),event_version.into(),kind.into(),payload.into()])).await?;
    Ok(())
}
impl Store {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
    pub async fn register(&self, actor: &SourceIdentity) -> KabiPayResult<Uuid> {
        let tx = self.db.begin().await?;
        let principal = Self::register_in(&tx, actor).await?;
        tx.commit().await?;
        Ok(principal)
    }
    pub(crate) async fn register_in(
        tx: &DatabaseTransaction,
        actor: &SourceIdentity,
    ) -> KabiPayResult<Uuid> {
        // Tenant -> account advisory lock -> principal/link is shared with bootstrap.
        // Bootstrap takes the tenant's UPDATE lock before entering this helper.
        one(tx,"SELECT id FROM kabipay_ops.tenant WHERE id=$1 AND status='ACTIVE' AND NOT is_deleted FOR SHARE",vec![actor.tenant().into()]).await?;
        tx.execute(sql(
            "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
            vec![format!("{}/{}", actor.tenant(), actor.user()).into()],
        ))
        .await?;
        let existing=tx.query_one(sql("SELECT id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND source_user_id=$2 AND issuer='kabipay-client'",vec![actor.tenant().into(),actor.user().into()])).await?;
        if existing.is_some() {
            let principal = current(tx, actor).await?;
            return Ok(principal.id);
        }
        let refs=tx.query_all(sql("SELECT DISTINCT principal_id FROM kabipay_ops.employment_ref WHERE source_tenant_id=$1 AND source_user_id=$2 AND principal_id IS NOT NULL",vec![actor.tenant().into(),actor.user().into()])).await?;
        if refs.len() > 1 {
            return Err(KabiPayError::Conflict(
                "source identity requires explicit reconciliation".into(),
            ));
        }
        let verified_ref = refs
            .first()
            .map(|r| r.try_get::<Uuid>("", "principal_id"))
            .transpose()?;
        let principal = verified_ref.unwrap_or_else(Uuid::new_v4);
        let link = Uuid::new_v4();
        if verified_ref.is_none() {
            tx.execute(sql(
                "INSERT INTO kabipay_ops.principal(id,display_name) VALUES($1,$2)",
                vec![principal.into(), actor.name().into()],
            ))
            .await?;
        } else {
            one(
                tx,
                "SELECT id FROM kabipay_ops.principal WHERE id=$1 AND status='ACTIVE' FOR SHARE",
                vec![principal.into()],
            )
            .await?;
        }
        tx.execute(sql("INSERT INTO kabipay_ops.principal_login_link(id,principal_id,source_tenant_id,source_user_id,issuer,status,verified_at) VALUES($1,$2,$3,$4,'kabipay-client','VERIFIED',NOW())",vec![link.into(),principal.into(),actor.tenant().into(),actor.user().into()])).await?;
        event(tx,actor,principal,EventSubject{organization:None,id:principal,version:1},"identity.registered",serde_json::json!({"principal_id":principal,"link_id":link,"display_name":actor.name(),"status":"VERIFIED"})).await?;
        Ok(principal)
    }
    pub async fn organizations(&self, actor: &SourceIdentity) -> KabiPayResult<Vec<Organization>> {
        let tx = self.db.begin().await?;
        let principal = current(&tx, actor).await?;
        let rows=tx.query_all(sql("SELECT DISTINCT o.id,o.name,o.status,o.version FROM kabipay_ops.organization o JOIN kabipay_ops.organization_company m ON m.organization_id=o.id WHERE m.tenant_id=$1 AND m.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND o.status='ACTIVE' ORDER BY o.id LIMIT 101",vec![actor.tenant().into()])).await?;
        let mut result = Vec::new();
        for row in rows {
            let id = row.try_get("", "id")?;
            if authorized(&tx, actor, principal.id, id, MANAGE)
                .await
                .is_ok()
            {
                result.push(Organization {
                    id,
                    name: row.try_get("", "name")?,
                    status: row.try_get("", "status")?,
                    version: row.try_get("", "version")?,
                });
            }
        }
        tx.commit().await?;
        Ok(result)
    }
    pub async fn revoke_own_identity(
        &self,
        actor: &SourceIdentity,
        version: i64,
    ) -> KabiPayResult<Uuid> {
        let tx = self.db.begin().await?;
        let principal = current(&tx, actor).await?;
        if principal.link_version != version {
            return Err(conflict());
        }
        let changed=tx.execute(sql("UPDATE kabipay_ops.principal_login_link SET status='REVOKED',version=version+1,updated_at=NOW() WHERE id=$1 AND version=$2 AND status='VERIFIED'",vec![principal.link.into(),version.into()])).await?;
        if changed.rows_affected() != 1 {
            return Err(conflict());
        }
        event(
            &tx,
            actor,
            principal.id,
            EventSubject {
                organization: None,
                id: principal.link,
                version: version + 1,
            },
            "identity.revoked",
            serde_json::json!({"link_id":principal.link,"status":"REVOKED"}),
        )
        .await?;
        tx.commit().await?;
        Ok(principal.link)
    }
}
