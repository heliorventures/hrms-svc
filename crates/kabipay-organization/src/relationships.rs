//! Serialized organization-wide reporting graph reservations and source installation.
use crate::graph_validation::validate_graph;
use crate::{identity::SourceIdentity, policy::MANAGE, store::*, workspace::*};
use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_domain::EffectivePeriod;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;
pub struct RelationshipProposal {
    pub organization_id: Uuid,
    pub expected_graph_version: i64,
    pub subject_employment_ref_id: Uuid,
    pub manager_employment_ref_id: Uuid,
    pub relationship_type: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub capabilities: Vec<String>,
}
const ASSIGNMENT_CAPS: &[&str] = &[
    "employee:read",
    "leave:read",
    "leave:approve",
    "timesheet:read",
    "timesheet:approve",
    "performance:read",
    "performance:evaluate",
    "feedback:write",
];
pub(crate) fn validate_relationship(p: &RelationshipProposal) -> KabiPayResult<()> {
    for id in [
        p.organization_id,
        p.subject_employment_ref_id,
        p.manager_employment_ref_id,
    ] {
        valid_id(id)?;
    }
    if p.expected_graph_version <= 0
        || p.expected_graph_version == i64::MAX
        || p.subject_employment_ref_id == p.manager_employment_ref_id
        || !["PRIMARY", "DOTTED_LINE", "PROJECT", "REVIEWER"]
            .contains(&p.relationship_type.as_str())
        || p.capabilities.is_empty()
        || p.capabilities.len() > ASSIGNMENT_CAPS.len()
        || p.capabilities
            .iter()
            .any(|c| !ASSIGNMENT_CAPS.contains(&c.as_str()))
        || p.capabilities
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != p.capabilities.len()
    {
        return Err(KabiPayError::Validation(
            "invalid reporting relationship or capabilities".into(),
        ));
    }
    EffectivePeriod::new(p.effective_from, p.effective_to)
        .map_err(|_| KabiPayError::Validation("invalid effective period".into()))?;
    Ok(())
}
pub(crate) async fn graph_lock(
    tx: &sea_orm::DatabaseTransaction,
    org: Uuid,
    expected: i64,
) -> KabiPayResult<i64> {
    // All administration already takes this row; one lock order prevents graph/admin deadlocks.
    one(
        tx,
        "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
        vec![org.into()],
    )
    .await?;
    tx.execute(sql("INSERT INTO kabipay_ops.organization_graph(organization_id) VALUES($1) ON CONFLICT DO NOTHING",vec![org.into()])).await?;
    let v: i64 = one(
        tx,
        "SELECT version FROM kabipay_ops.organization_graph WHERE organization_id=$1 FOR UPDATE",
        vec![org.into()],
    )
    .await?
    .try_get("", "version")?;
    if expected != v || v == i64::MAX {
        return Err(conflict());
    }
    Ok(v)
}
impl Store {
    pub async fn relationships(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let rows=tx.query_all(sql("SELECT a.id,to_jsonb(a)||jsonb_build_object('membership_id',a.organization_company_id,'company_name',t.name,'subject_display_name',s.display_name,'manager_display_name',m.display_name,'manager_tenant_id',m.source_tenant_id,'manager_company_name',mt.name) AS data FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref s ON s.id=a.subject_employment_ref_id AND s.organization_id=a.organization_id JOIN kabipay_ops.employment_ref m ON m.id=a.manager_employment_ref_id AND m.organization_id=a.organization_id JOIN kabipay_ops.tenant t ON t.id=a.source_tenant_id JOIN kabipay_ops.tenant mt ON mt.id=m.source_tenant_id WHERE a.organization_id=$1 AND ($2::uuid IS NULL OR a.id>$2) ORDER BY a.id LIMIT $3",vec![org.into(),after.into(),i64::from(limit+1).into()])).await?;
        let items = rows
            .iter()
            .map(|r| r.try_get("", "data"))
            .collect::<Result<Vec<Value>, _>>()?;
        let v = graph_version(&tx, org).await?;
        tx.commit().await?;
        page(items, limit, v)
    }
    pub async fn propose_relationship(
        &self,
        actor: &SourceIdentity,
        p: RelationshipProposal,
    ) -> KabiPayResult<Value> {
        validate_relationship(&p)?;
        let tx = self.db.begin().await?;
        let v = graph_lock(&tx, p.organization_id, p.expected_graph_version).await?;
        let principal = current(&tx, actor).await?;
        source_member(&tx, actor, p.organization_id).await?;
        authorized(&tx, actor, principal.id, p.organization_id, MANAGE).await?;
        let subject=one(&tx,"SELECT e.organization_company_id,e.source_tenant_id,e.principal_id FROM kabipay_ops.employment_ref e JOIN kabipay_ops.organization_company c ON c.id=e.organization_company_id AND c.organization_id=e.organization_id JOIN kabipay_ops.principal p ON p.id=e.principal_id WHERE e.id=$1 AND e.organization_id=$2 AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND p.status='ACTIVE' AND c.status='ACTIVE' AND c.effective_from<=NOW() AND (c.effective_to IS NULL OR NOW()<c.effective_to) FOR SHARE OF e,p,c",vec![p.subject_employment_ref_id.into(),p.organization_id.into()]).await?;
        let manager=one(&tx,"SELECT e.principal_id FROM kabipay_ops.employment_ref e JOIN kabipay_ops.organization_company c ON c.id=e.organization_company_id AND c.organization_id=e.organization_id JOIN kabipay_ops.principal p ON p.id=e.principal_id WHERE e.id=$1 AND e.organization_id=$2 AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND p.status='ACTIVE' AND c.status='ACTIVE' AND c.effective_from<=NOW() AND (c.effective_to IS NULL OR NOW()<c.effective_to) FOR SHARE OF e,p,c",vec![p.manager_employment_ref_id.into(),p.organization_id.into()]).await?;
        if subject.try_get::<Uuid>("", "principal_id")?
            == manager.try_get::<Uuid>("", "principal_id")?
        {
            return Err(KabiPayError::Validation(
                "a person cannot manage their own employment".into(),
            ));
        }
        let id = Uuid::new_v4();
        let member: Uuid = subject.try_get("", "organization_company_id")?;
        let tenant: Uuid = subject.try_get("", "source_tenant_id")?;
        tx.execute(sql("INSERT INTO kabipay_ops.organization_relationship(id,organization_id,organization_company_id,source_tenant_id,subject_employment_ref_id,manager_employment_ref_id,relationship_type,status,effective_from,effective_to,graph_version,capabilities,created_by) VALUES($1,$2,$3,$4,$5,$6,$7,'PENDING',$8,$9,$10,$11,$12)",vec![id.into(),p.organization_id.into(),member.into(),tenant.into(),p.subject_employment_ref_id.into(),p.manager_employment_ref_id.into(),p.relationship_type.clone().into(),p.effective_from.into(),p.effective_to.into(),(v+1).into(),json!(p.capabilities).into(),principal.id.into()])).await?;
        validate_graph(&tx, p.organization_id).await?;
        tx.execute(sql(
            "UPDATE kabipay_ops.organization_graph SET version=version+1 WHERE organization_id=$1",
            vec![p.organization_id.into()],
        ))
        .await?;
        event(&tx,actor,principal.id,EventSubject{organization:Some(p.organization_id),id,version:1},"relationship.proposed",json!({"id":id,"subject":p.subject_employment_ref_id,"manager":p.manager_employment_ref_id,"graph_version":v+1})).await?;
        tx.commit().await?;
        Ok(
            json!({"id":id,"membership_id":member,"tenant_id":tenant,"status":"PENDING","version":1,"graph_version":v+1}),
        )
    }

    /// Replace a manager atomically under the same assignment ID. The previously
    /// installed snapshot stays reserved until the source ACKs the new revision.
    pub async fn change_relationship_manager(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        id: Uuid,
        expected: i64,
        expected_graph: i64,
        manager: Uuid,
    ) -> KabiPayResult<Value> {
        valid_id(id)?;
        valid_id(manager)?;
        if expected <= 0 || expected == i64::MAX {
            return Err(conflict());
        }
        let tx = self.db.begin().await?;
        let graph = graph_lock(&tx, org, expected_graph).await?;
        require_known_installation(&tx, org, id).await?;
        let p = current(&tx, actor).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let old=one(&tx,"SELECT a.subject_employment_ref_id,s.principal_id,a.organization_company_id FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref s ON s.id=a.subject_employment_ref_id AND s.organization_id=a.organization_id JOIN kabipay_ops.organization_company c ON c.id=a.organization_company_id WHERE a.id=$1 AND a.organization_id=$2 AND a.version=$3 AND a.status IN ('ACTIVE','PENDING') AND c.status='ACTIVE' FOR UPDATE OF a",vec![id.into(),org.into(),expected.into()]).await?;
        let m=one(&tx,"SELECT e.principal_id FROM kabipay_ops.employment_ref e JOIN kabipay_ops.organization_company c ON c.id=e.organization_company_id JOIN kabipay_ops.principal p ON p.id=e.principal_id WHERE e.id=$1 AND e.organization_id=$2 AND e.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND c.status='ACTIVE' AND p.status='ACTIVE' AND c.effective_from<=NOW() AND (c.effective_to IS NULL OR NOW()<c.effective_to) FOR SHARE OF e,c,p",vec![manager.into(),org.into()]).await?;
        if old.try_get::<Uuid>("", "principal_id")? == m.try_get::<Uuid>("", "principal_id")? {
            return Err(KabiPayError::Validation(
                "a person cannot manage their own employment".into(),
            ));
        }
        tx.execute(sql("UPDATE kabipay_ops.organization_relationship SET manager_employment_ref_id=$3,version=version+1,graph_version=$4,status='PENDING',updated_at=NOW() WHERE id=$1 AND organization_id=$2",vec![id.into(),org.into(),manager.into(),(graph+1).into()])).await?;
        validate_graph(&tx, org).await?;
        tx.execute(sql(
            "UPDATE kabipay_ops.organization_graph SET version=version+1 WHERE organization_id=$1",
            vec![org.into()],
        ))
        .await?;
        event(
            &tx,
            actor,
            p.id,
            EventSubject {
                organization: Some(org),
                id,
                version: expected + 1,
            },
            "relationship.manager_changed",
            json!({"id":id,"manager":manager,"version":expected+1,"graph_version":graph+1}),
        )
        .await?;
        let membership: Uuid = old.try_get("", "organization_company_id")?;
        tx.commit().await?;
        Ok(
            json!({"id":id,"membership_id":membership,"status":"PENDING","version":expected+1,"graph_version":graph+1}),
        )
    }
    pub async fn end_relationship(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        id: Uuid,
        version: i64,
        expected_graph: i64,
    ) -> KabiPayResult<Value> {
        if version <= 0 || version == i64::MAX {
            return Err(conflict());
        }
        let tx = self.db.begin().await?;
        let graph = graph_lock(&tx, org, expected_graph).await?;
        require_known_installation(&tx, org, id).await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let change=tx.execute(sql("UPDATE kabipay_ops.organization_relationship SET status='ENDED',version=version+1,graph_version=$4,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND version=$3 AND status IN ('PENDING','ACTIVE')",vec![id.into(),org.into(),version.into(),(graph+1).into()])).await?;
        if change.rows_affected() != 1 {
            return Err(conflict());
        }
        validate_graph(&tx, org).await?;
        tx.execute(sql(
            "UPDATE kabipay_ops.organization_graph SET version=version+1 WHERE organization_id=$1",
            vec![org.into()],
        ))
        .await?;
        event(
            &tx,
            actor,
            p.id,
            EventSubject {
                organization: Some(org),
                id,
                version: version + 1,
            },
            "relationship.ended",
            json!({"id":id,"status":"ENDED","graph_version":graph+1}),
        )
        .await?;
        tx.commit().await?;
        Ok(json!({"id":id,"status":"ENDED","version":version+1,"graph_version":graph+1}))
    }
}
async fn require_known_installation(
    tx: &sea_orm::DatabaseTransaction,
    org: Uuid,
    id: Uuid,
) -> KabiPayResult<()> {
    let pending=tx.query_one(sql("SELECT i.command_id FROM kabipay_ops.organization_command_intent i JOIN kabipay_ops.organization_relationship a ON a.id=i.resource_id AND a.organization_id=i.organization_id WHERE a.id=$1 AND a.organization_id=$2 AND i.operation='install_relationship' AND i.status IN ('SUBMITTED','AWAITING_CONFIRMATION') AND (a.installed_version IS DISTINCT FROM a.version OR a.installed_hash IS NULL) LIMIT 1",vec![id.into(),org.into()])).await?;
    if pending.is_some() {
        return Err(KabiPayError::ConflictRule{code:"ORGANIZATION_INSTALLATION_UNCONFIRMED",message:"Confirm the pending source relationship installation before changing this assignment.".into()});
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relationship_input_requires_explicit_non_privileged_capabilities() {
        let mut p = RelationshipProposal {
            organization_id: Uuid::new_v4(),
            expected_graph_version: 1,
            subject_employment_ref_id: Uuid::new_v4(),
            manager_employment_ref_id: Uuid::new_v4(),
            relationship_type: "PRIMARY".into(),
            effective_from: Utc::now(),
            effective_to: None,
            capabilities: vec!["leave:approve".into()],
        };
        assert!(validate_relationship(&p).is_ok());
        p.capabilities = vec!["payroll:manage".into()];
        assert!(validate_relationship(&p).is_err());
        p.capabilities = vec!["leave:approve".into(); 2];
        assert!(validate_relationship(&p).is_err());
    }
}
