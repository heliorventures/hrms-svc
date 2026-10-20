use crate::{
    execution::Execution, identity::SourceIdentity, policy::MANAGE, store::*, workspace::*,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{OrganizationCommand, OrganizationOperation};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryResult {
    employees: Vec<DirectoryEmployee>,
    after: Option<Uuid>,
    has_more: bool,
    #[serde(default)]
    source_bootstrap: Option<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryEmployee {
    employee_id: Uuid,
    user_id: Option<Uuid>,
    display_name: String,
    employee_code: Option<String>,
    status: String,
    department_id: Option<Uuid>,
    designation_id: Option<Uuid>,
    version: i64,
    #[serde(default)]
    is_deleted: bool,
    #[serde(default)]
    reporting_manager_id: Option<Uuid>,
}
impl Execution {
    pub async fn sync_directory(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        self.ensure_membership(actor, org, member).await?;
        let tx = self.store.db.begin().await?;
        one(
            &tx,
            "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
            vec![org.into()],
        )
        .await?;
        let p = current(&tx, actor).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let (_, mv) = crate::runtime_policy::membership(&tx, org, member).await?;
        let run = if let Some(after) = after {
            let row=one(&tx,"SELECT run_id FROM kabipay_ops.organization_directory_sync WHERE organization_id=$1 AND membership_id=$2 AND membership_version=$3 AND status='RUNNING' AND source_after=$4 FOR UPDATE",vec![org.into(),member.into(),mv.into(),after.into()]).await?;
            row.try_get::<Uuid>("", "run_id")?
        } else {
            let run = Uuid::new_v4();
            tx.execute(sql("INSERT INTO kabipay_ops.organization_directory_sync(membership_id,organization_id,membership_version,run_id,source_after,status) VALUES($1,$2,$3,$4,NULL,'RUNNING') ON CONFLICT(membership_id) DO UPDATE SET membership_version=excluded.membership_version,run_id=excluded.run_id,source_after=NULL,status='RUNNING',updated_at=NOW()",vec![member.into(),org.into(),mv.into(),run.into()])).await?;
            run
        };
        tx.commit().await?;
        let outcome = self
            .execute(
                actor,
                org,
                member,
                OrganizationCommand {
                    command_id: Uuid::new_v4(),
                    resource_id: member,
                    expected_version: mv,
                    operation: OrganizationOperation::Directory,
                    payload: serde_json::json!({"after":after,"first":limit}),
                },
            )
            .await?;
        if outcome.status != "COMMITTED" {
            return Err(KabiPayError::Internal(
                "company directory is unavailable; retry after checking source status".into(),
            ));
        }
        let result: DirectoryResult = serde_json::from_value(outcome.result)?;
        if result.employees.len() > limit as usize || result.has_more && result.after.is_none() {
            return Err(KabiPayError::Internal(
                "invalid company directory pagination".into(),
            ));
        }
        let tx = self.store.db.begin().await?;
        one(
            &tx,
            "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
            vec![org.into()],
        )
        .await?;
        let p = current(&tx, actor).await?;
        authorized(&tx, actor, p.id, org, MANAGE).await?;
        let (tenant, version) = crate::runtime_policy::membership(&tx, org, member).await?;
        if mv != version {
            return Err(conflict());
        }
        one(&tx,"SELECT membership_id FROM kabipay_ops.organization_directory_sync WHERE organization_id=$1 AND membership_id=$2 AND membership_version=$3 AND run_id=$4 AND source_after IS NOT DISTINCT FROM $5 AND status='RUNNING' FOR UPDATE",vec![org.into(),member.into(),mv.into(),run.into(),after.into()]).await?;
        let mut last = after;
        let mut ids = Vec::new();
        for mut e in result.employees {
            if e.is_deleted {
                e.status = "TERMINATED".into();
            }
            valid_id(e.employee_id)?;
            valid_name(&e.display_name, 255)?;
            if e.version <= 0
                || last.is_some_and(|id| e.employee_id <= id)
                || e.user_id.is_some_and(|id| id.is_nil())
                || ![
                    "ACTIVE",
                    "PROBATION",
                    "ON_LEAVE",
                    "INACTIVE",
                    "SUSPENDED",
                    "TERMINATED",
                ]
                .contains(&e.status.as_str())
                || e.employee_code.as_ref().is_some_and(|v| v.len() > 100)
            {
                return Err(KabiPayError::Internal(
                    "invalid source employee directory".into(),
                ));
            }
            last = Some(e.employee_id);
            if let Some(user) = e.user_id {
                tx.execute(sql(
                    "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                    vec![format!("{tenant}/{user}").into()],
                ))
                .await?;
            }
            let old=tx.query_one(sql("SELECT id,principal_id,source_user_id,source_version FROM kabipay_ops.employment_ref WHERE organization_company_id=$1 AND source_employee_id=$2 FOR UPDATE",vec![member.into(),e.employee_id.into()])).await?;
            let linked = if let Some(user) = e.user_id {
                tx.query_one(sql("SELECT principal_id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND source_user_id=$2 AND issuer='kabipay-client'",vec![tenant.into(),user.into()])).await?.map(|r|r.try_get::<Uuid>("","principal_id")).transpose()?
            } else {
                None
            };
            let (id, principal) = if let Some(old) = old {
                let v: i64 = old.try_get("", "source_version")?;
                if e.version < v {
                    return Err(conflict());
                }
                let principal: Option<Uuid> = old.try_get("", "principal_id")?;
                let user: Option<Uuid> = old.try_get("", "source_user_id")?;
                if user.is_some() && user != e.user_id
                    || linked.is_some() && principal.is_some() && linked != principal
                {
                    return Err(KabiPayError::Conflict("verified employee identity changed; explicit identity reconciliation is required".into()));
                }
                (old.try_get("", "id")?, principal.or(linked))
            } else {
                (Uuid::new_v4(), linked)
            };
            let principal = if let Some(principal) = principal {
                principal
            } else {
                let id = Uuid::new_v4();
                tx.execute(sql(
                    "INSERT INTO kabipay_ops.principal(id,display_name) VALUES($1,$2)",
                    vec![id.into(), e.display_name.clone().into()],
                ))
                .await?;
                id
            };
            tx.execute(sql("INSERT INTO kabipay_ops.employment_ref(id,organization_id,organization_company_id,source_tenant_id,source_employee_id,principal_id,display_name,employment_status,source_version,synced_at,source_user_id,department_id,designation_id,employee_code,legacy_manager_employee_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,NOW(),$10,$11,$12,$13,$14) ON CONFLICT(organization_company_id,source_employee_id) DO UPDATE SET display_name=excluded.display_name,employment_status=excluded.employment_status,source_version=excluded.source_version,synced_at=NOW(),source_user_id=excluded.source_user_id,department_id=excluded.department_id,designation_id=excluded.designation_id,employee_code=excluded.employee_code,legacy_manager_employee_id=excluded.legacy_manager_employee_id WHERE employment_ref.source_version<=excluded.source_version",vec![id.into(),org.into(),member.into(),tenant.into(),e.employee_id.into(),principal.into(),e.display_name.into(),e.status.into(),e.version.into(),e.user_id.into(),e.department_id.into(),e.designation_id.into(),e.employee_code.into(),e.reporting_manager_id.into()])).await?;
            ids.push(id);
        }
        if result.has_more && result.after != last {
            return Err(KabiPayError::Internal(
                "invalid source directory cursor".into(),
            ));
        }
        event(&tx,actor,p.id,EventSubject{organization:Some(org),id:member,version:mv},"directory.synchronized",serde_json::json!({"membership_id":member,"records":ids.len(),"source_after":result.after,"has_more":result.has_more,"source_bootstrap":result.source_bootstrap})).await?;
        tx.execute(sql("UPDATE kabipay_ops.organization_directory_sync SET source_after=$3,status=$4,updated_at=NOW() WHERE organization_id=$1 AND membership_id=$2 AND run_id=$5",vec![org.into(),member.into(),result.after.into(),if result.has_more{"RUNNING".into()}else{"COMPLETE".into()},run.into()])).await?;
        let graph = graph_version(&tx, org).await?;
        tx.commit().await?;
        // A sync result cursor is a SOURCE employee cursor, separate from the
        // central directory query's employment-ref cursor.
        let items = ids
            .into_iter()
            .map(|id| serde_json::json!({"id":id}))
            .collect();
        Ok(OrganizationDocumentPage {
            schema_version: 1,
            items: async_graphql::Json(items),
            end_cursor: result.after,
            has_next_page: result.has_more,
            graph_version: graph,
        })
    }
}
