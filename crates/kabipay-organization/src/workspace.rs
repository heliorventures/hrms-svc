//! Current organization access, independent of tenant-local HR roles.
use crate::{
    identity::SourceIdentity,
    policy::{DELEGATE, MANAGE},
    store::*,
};
use async_graphql::{Json, SimpleObject};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;
#[derive(SimpleObject)]
pub struct OrganizationWorkspace {
    pub id: Uuid,
    pub name: String,
    pub version: i64,
    pub can_manage: bool,
    pub can_delegate: bool,
    pub can_approve: bool,
    pub can_review: bool,
    pub can_report: bool,
    pub can_history: bool,
    pub can_payroll: bool,
}
#[derive(SimpleObject)]
pub struct OrganizationCompany {
    pub membership_id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub status: String,
    pub version: i64,
    pub capabilities: Vec<String>,
    pub runtime_status: String,
    pub installed_version: Option<i64>,
    pub directory_sync_status: String,
}
#[derive(SimpleObject)]
pub struct OrganizationDocumentPage {
    pub schema_version: i32,
    pub items: Json<Vec<Value>>,
    pub end_cursor: Option<Uuid>,
    pub has_next_page: bool,
    pub graph_version: i64,
}
pub(crate) fn page(
    rows: Vec<Value>,
    limit: i32,
    graph_version: i64,
) -> KabiPayResult<OrganizationDocumentPage> {
    let more = rows.len() > limit as usize;
    let mut rows = rows;
    if more {
        rows.pop();
    }
    let cursor = if more {
        rows.last()
            .and_then(|v| v["id"].as_str())
            .and_then(|v| Uuid::parse_str(v).ok())
    } else {
        None
    };
    Ok(OrganizationDocumentPage {
        schema_version: 1,
        items: Json(rows),
        end_cursor: cursor,
        has_next_page: more,
        graph_version,
    })
}
pub(crate) fn limit_page(after: Option<Uuid>, limit: i32) -> KabiPayResult<()> {
    if !(1..=100).contains(&limit) || after.is_some_and(|x| x.is_nil()) {
        Err(KabiPayError::Validation(
            "valid cursor and page size 1..100 required".into(),
        ))
    } else {
        Ok(())
    }
}
pub(crate) async fn source_member(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    org: Uuid,
) -> KabiPayResult<()> {
    one(tx,"SELECT o.id FROM kabipay_ops.organization o JOIN kabipay_ops.organization_company m ON m.organization_id=o.id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE o.id=$1 AND o.status='ACTIVE' AND m.tenant_id=$2 AND m.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND t.status='ACTIVE' AND NOT t.is_deleted",vec![org.into(),actor.tenant().into()]).await?;
    Ok(())
}
pub(crate) async fn capabilities(
    tx: &DatabaseTransaction,
    principal: Uuid,
    org: Uuid,
    membership: Uuid,
) -> KabiPayResult<BTreeSet<String>> {
    let rows=tx.query_all(sql("SELECT DISTINCT c.capability_code AS code FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id JOIN kabipay_ops.organization_company m ON m.id=s.organization_company_id AND m.organization_id=b.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE (c.capability_code NOT IN ('payroll:manage','payroll:read','analytics:read','history:read','history:export','employee:manage','role:manage','workflow:manage','onboarding:manage') OR c.employee_scope='ALL') AND b.organization_id=$1 AND b.principal_id=$2 AND b.status='ACTIVE' AND r.status='ACTIVE' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND s.organization_company_id=$3 AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND m.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND t.status='ACTIVE' AND NOT t.is_deleted",vec![org.into(),principal.into(),membership.into()])).await?;
    let mut caps = rows
        .iter()
        .map(|r| r.try_get("", "code"))
        .collect::<Result<BTreeSet<String>, _>>()?;
    let rows=tx.query_all(sql("SELECT a.capabilities FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id AND manager.organization_id=a.organization_id JOIN kabipay_ops.organization_company mm ON mm.id=manager.organization_company_id JOIN kabipay_ops.organization_company sm ON sm.id=a.organization_company_id JOIN kabipay_ops.employment_ref subject ON subject.id=a.subject_employment_ref_id WHERE a.organization_id=$1 AND manager.principal_id=$2 AND a.organization_company_id=$3 AND a.status='ACTIVE' AND a.installed_version=a.version AND a.effective_from<=NOW() AND (a.effective_to IS NULL OR NOW()<a.effective_to) AND mm.status='ACTIVE' AND mm.effective_from<=NOW() AND (mm.effective_to IS NULL OR NOW()<mm.effective_to) AND sm.status='ACTIVE' AND sm.effective_from<=NOW() AND (sm.effective_to IS NULL OR NOW()<sm.effective_to) AND manager.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![org.into(),principal.into(),membership.into()])).await?;
    for r in rows {
        let v: Value = r.try_get("", "capabilities")?;
        for c in v.as_array().ok_or_else(denied)? {
            caps.insert(c.as_str().ok_or_else(denied)?.into());
        }
    }
    Ok(caps)
}
pub(crate) async fn graph_version(tx: &DatabaseTransaction, org: Uuid) -> KabiPayResult<i64> {
    Ok(tx
        .query_one(sql(
            "SELECT version FROM kabipay_ops.organization_graph WHERE organization_id=$1",
            vec![org.into()],
        ))
        .await?
        .map(|r| r.try_get("", "version"))
        .transpose()?
        .unwrap_or(1))
}
pub(crate) async fn company_rows(
    tx: &DatabaseTransaction,
    actor: &SourceIdentity,
    p: Uuid,
    org: Uuid,
) -> KabiPayResult<Vec<OrganizationCompany>> {
    source_member(tx, actor, org).await?;
    let manage = authorized(tx, actor, p, org, MANAGE).await.is_ok();
    let delegate = authorized(tx, actor, p, org, DELEGATE).await.is_ok();
    let rows=tx.query_all(sql("SELECT m.id,m.tenant_id,m.status,m.version,m.runtime_status,m.installed_version,COALESCE((SELECT CASE WHEN d.status='COMPLETE' AND (m.status<>'ACTIVE' OR d.membership_version=m.version) THEN 'COMPLETE' ELSE d.status END FROM kabipay_ops.organization_directory_sync d WHERE d.membership_id=m.id),'NOT_STARTED') AS directory_sync_status,t.name FROM kabipay_ops.organization_company m JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE m.organization_id=$1 AND NOT t.is_deleted ORDER BY m.id LIMIT 101",vec![org.into()])).await?;
    if rows.len() > 100 {
        return Err(KabiPayError::Validation(
            "organization company limit exceeded".into(),
        ));
    }
    let mut result = Vec::new();
    for r in rows {
        let id = r.try_get("", "id")?;
        let mut caps = capabilities(tx, p, org, id).await?;
        if manage {
            caps.insert(MANAGE.into());
        }
        if delegate {
            caps.insert(DELEGATE.into());
        }
        if manage || delegate || !caps.is_empty() {
            result.push(OrganizationCompany {
                membership_id: id,
                tenant_id: r.try_get("", "tenant_id")?,
                name: r.try_get("", "name")?,
                status: r.try_get("", "status")?,
                version: r.try_get("", "version")?,
                capabilities: caps.into_iter().collect(),
                runtime_status: r.try_get("", "runtime_status")?,
                installed_version: r.try_get("", "installed_version")?,
                directory_sync_status: r.try_get("", "directory_sync_status")?,
            });
        }
    }
    Ok(result)
}
impl Store {
    pub async fn workspaces(
        &self,
        actor: &SourceIdentity,
    ) -> KabiPayResult<Vec<OrganizationWorkspace>> {
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        let rows=tx.query_all(sql("SELECT DISTINCT o.id,o.name,o.version FROM kabipay_ops.organization o JOIN kabipay_ops.organization_company m ON m.organization_id=o.id WHERE m.tenant_id=$1 AND m.status='ACTIVE' AND o.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) ORDER BY o.id LIMIT 101",vec![actor.tenant().into()])).await?;
        let mut items = Vec::new();
        for r in rows {
            let id = r.try_get("", "id")?;
            let companies = company_rows(&tx, actor, p.id, id).await?;
            let caps: BTreeSet<String> = companies
                .iter()
                .flat_map(|m| m.capabilities.iter().cloned())
                .collect();
            if caps.is_empty() {
                continue;
            }
            items.push(OrganizationWorkspace {
                id,
                name: r.try_get("", "name")?,
                version: r.try_get("", "version")?,
                can_manage: authorized(&tx, actor, p.id, id, MANAGE).await.is_ok(),
                can_delegate: authorized(&tx, actor, p.id, id, DELEGATE).await.is_ok(),
                can_approve: caps.contains("leave:approve") || caps.contains("timesheet:approve"),
                can_review: caps.contains("performance:evaluate")
                    || caps.contains("feedback:write"),
                can_report: caps.contains("analytics:read"),
                can_history: caps.contains("history:read"),
                can_payroll: caps.contains("payroll:manage"),
            });
        }
        tx.commit().await?;
        Ok(items)
    }
    pub async fn companies(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
    ) -> KabiPayResult<Vec<OrganizationCompany>> {
        valid_id(org)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        let rows = company_rows(&tx, actor, p.id, org).await?;
        tx.commit().await?;
        Ok(rows)
    }
    pub async fn invitations(
        &self,
        actor: &SourceIdentity,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        if !actor.may_accept_company() {
            return Err(denied());
        }
        let rows=self.db.query_all(sql("SELECT m.id,jsonb_build_object('id',m.id,'organization_id',o.id,'organization_name',o.name,'organization_version',o.version,'tenant_id',m.tenant_id,'status',m.status,'version',m.version) AS data FROM kabipay_ops.organization_company m JOIN kabipay_ops.organization o ON o.id=m.organization_id WHERE m.tenant_id=$1 AND m.status='PENDING' AND o.status='ACTIVE' ORDER BY m.id LIMIT 101",vec![actor.tenant().into()])).await?;
        page(
            rows.iter()
                .map(|r| r.try_get("", "data"))
                .collect::<Result<_, _>>()?,
            100,
            1,
        )
    }
}
