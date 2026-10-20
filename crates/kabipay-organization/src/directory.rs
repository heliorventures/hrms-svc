use crate::{identity::SourceIdentity, policy::MANAGE, store::*, workspace::*};
use kabipay_common::KabiPayResult;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;
pub(crate) async fn may_read_employee(
    tx: &DatabaseTransaction,
    p: Uuid,
    org: Uuid,
    membership: Uuid,
    employee: Uuid,
    cap: &str,
) -> KabiPayResult<bool> {
    Ok(
        crate::runtime_policy::resolve_authority(
            tx,
            p,
            org,
            membership,
            cap,
            Some(employee),
            false,
        )
        .await
        .is_ok(),
    )
}
impl Store {
    pub async fn directory(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership: Uuid,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        let manage = authorized(&tx, actor, p.id, org, MANAGE).await.is_ok()
            || authorized(&tx, actor, p.id, org, crate::policy::DELEGATE)
                .await
                .is_ok();
        if !manage {
            let mut allowed = false;
            for capability in ["employee:read", "employee:manage", "onboarding:manage"] {
                match crate::runtime_policy::resolve_authority(
                    &tx, p.id, org, membership, capability, None, true,
                )
                .await
                {
                    Ok(_) => {
                        allowed = true;
                        break;
                    }
                    Err(kabipay_common::KabiPayError::Forbidden(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            if !allowed {
                return Err(denied());
            }
        }
        let rows=tx.query_all(sql("SELECT e.id,jsonb_build_object('id',e.id,'employment_ref_id',e.id,'principal_id',e.principal_id,'employee_id',e.source_employee_id,'membership_id',e.organization_company_id,'tenant_id',e.source_tenant_id,'company_name',t.name,'display_name',e.display_name,'employee_code',e.employee_code,'department_id',e.department_id,'designation_id',e.designation_id,'status',e.employment_status,'version',e.source_version) AS data FROM kabipay_ops.employment_ref e JOIN kabipay_ops.organization_company m ON m.id=e.organization_company_id AND m.organization_id=e.organization_id JOIN kabipay_ops.tenant t ON t.id=e.source_tenant_id WHERE e.organization_id=$1 AND e.organization_company_id=$2 AND m.status='ACTIVE' AND ($3::uuid IS NULL OR e.id>$3) ORDER BY e.id LIMIT $4",vec![org.into(),membership.into(),after.into(),i64::from(limit+1).into()])).await?;
        let data = rows
            .iter()
            .map(|r| r.try_get("", "data"))
            .collect::<Result<Vec<Value>, _>>()?;
        let graph = graph_version(&tx, org).await?;
        tx.commit().await?;
        page(data, limit, graph)
    }
    pub async fn team(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership: Option<Uuid>,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        // Filter with current installed manager edges before pagination. Administrator
        // metadata rights alone do not expose employee business records.
        let rows=tx.query_all(sql("SELECT e.id,jsonb_build_object('id',e.id,'employment_ref_id',e.id,'principal_id',e.principal_id,'employee_id',e.source_employee_id,'membership_id',e.organization_company_id,'tenant_id',e.source_tenant_id,'company_name',t.name,'display_name',e.display_name,'status',e.employment_status,'version',e.source_version) AS data FROM kabipay_ops.employment_ref e JOIN kabipay_ops.organization_company m ON m.id=e.organization_company_id JOIN kabipay_ops.tenant t ON t.id=e.source_tenant_id WHERE e.organization_id=$1 AND m.status='ACTIVE' AND m.installed_version=m.version AND m.runtime_status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND t.status='ACTIVE' AND NOT t.is_deleted AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND ($2::uuid IS NULL OR m.id=$2) AND ($3::uuid IS NULL OR e.id>$3) AND (EXISTS(SELECT 1 FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id JOIN kabipay_ops.organization_company mm ON mm.id=manager.organization_company_id WHERE a.organization_id=e.organization_id AND a.subject_employment_ref_id=e.id AND a.status='ACTIVE' AND a.installed_version=a.version AND a.effective_from<=NOW() AND (a.effective_to IS NULL OR NOW()<a.effective_to) AND manager.principal_id=$4 AND manager.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND mm.status='ACTIVE' AND mm.installed_version=mm.version AND mm.runtime_status='ACTIVE' AND mm.effective_from<=NOW() AND (mm.effective_to IS NULL OR NOW()<mm.effective_to)) OR EXISTS(SELECT 1 FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id WHERE b.organization_id=e.organization_id AND b.principal_id=$4 AND b.status='ACTIVE' AND r.status='ACTIVE' AND b.effective_from<=NOW() AND (b.effective_to IS NULL OR NOW()<b.effective_to) AND s.organization_company_id=m.id AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND c.capability_code IN ('employee:read','feedback:write') AND c.employee_scope='ALL')) ORDER BY e.id LIMIT $5",vec![org.into(),membership.into(),after.into(),p.id.into(),i64::from(limit+1).into()])).await?;
        let data = rows
            .iter()
            .map(|r| r.try_get("", "data"))
            .collect::<Result<Vec<Value>, _>>()?;
        let graph = graph_version(&tx, org).await?;
        tx.commit().await?;
        page(data, limit, graph)
    }
    pub async fn approvals(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership: Option<Uuid>,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        // A bounded scan cursor advances over unreadable entries as well. This avoids
        // leaking foreign records and does not lose later authorized work.
        let rows=tx.query_all(sql("SELECT i.id,i.resource_type,i.employee_id,m.id AS membership_id,jsonb_build_object('id',i.id,'resource_id',i.resource_id,'resource_type',i.resource_type,'membership_id',m.id,'tenant_id',m.tenant_id,'company_name',t.name,'employee_id',i.employee_id,'employee_name',i.employee_display_name,'display_name',i.employee_display_name,'status',i.status,'summary',i.summary,'version',i.source_version,'updated_at',i.occurred_at) AS data FROM kabipay_ops.organization_inbox_projection i JOIN kabipay_ops.organization_company m ON m.tenant_id=i.source_tenant_id AND m.organization_id=i.organization_id JOIN kabipay_ops.tenant t ON t.id=m.tenant_id WHERE i.organization_id=$1 AND m.status='ACTIVE' AND m.installed_version=m.version AND m.runtime_status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) AND t.status='ACTIVE' AND NOT t.is_deleted AND NOT i.deleted AND i.status IN ('PENDING','SUBMITTED','MANAGER_REVIEW') AND ($2::uuid IS NULL OR m.id=$2) AND ($3::uuid IS NULL OR i.id>$3) ORDER BY i.id LIMIT 501",vec![org.into(),membership.into(),after.into()])).await?;
        let mut data = Vec::new();
        let mut last_scanned = None;
        let mut more = false;
        for r in &rows {
            let id: Uuid = r.try_get("", "id")?;
            last_scanned = Some(id);
            let kind: String = r.try_get("", "resource_type")?;
            let cap = match kind.as_str() {
                "leave_request" => "leave:approve",
                "timesheet" => "timesheet:approve",
                "performance_participant" => "performance:evaluate",
                _ => continue,
            };
            let employee: Option<Uuid> = r.try_get("", "employee_id")?;
            let member: Uuid = r.try_get("", "membership_id")?;
            if let Some(employee) = employee {
                if may_read_employee(&tx, p.id, org, member, employee, cap).await? {
                    if data.len() == limit as usize {
                        more = true;
                        break;
                    }
                    data.push(r.try_get("", "data")?);
                }
            }
        }
        more |= rows.len() == 501;
        let graph = graph_version(&tx, org).await?;
        tx.commit().await?;
        let cursor = if more {
            if data.len() == limit as usize {
                data.last()
                    .and_then(|v: &Value| v["id"].as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            } else {
                last_scanned
            }
        } else {
            None
        };
        Ok(OrganizationDocumentPage {
            schema_version: 1,
            items: async_graphql::Json(data),
            end_cursor: cursor,
            has_next_page: more,
            graph_version: graph,
        })
    }
}
