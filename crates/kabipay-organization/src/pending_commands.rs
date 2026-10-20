//! Recovery references survive a browser reload without persisting private payloads.
use crate::{
    identity::SourceIdentity,
    runtime_policy::{authority, Authority},
    store::*,
    workspace::*,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::OrganizationOperation;
use kabipay_organization_security::AuthorityKind;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

fn same_authority(current: &Authority, kind: &str, id: Uuid, version: i64) -> bool {
    let expected_kind = match current.kind {
        AuthorityKind::OrganizationGrant => "organization_grant",
        AuthorityKind::ManagementAssignment => "management_assignment",
    };
    kind == expected_kind && current.id == id && current.version == version
}

fn recoverable_authority(
    current: KabiPayResult<Authority>,
    kind: &str,
    id: Uuid,
    version: i64,
) -> KabiPayResult<bool> {
    match current {
        Ok(current) => Ok(same_authority(&current, kind, id, version)),
        Err(KabiPayError::Forbidden(_)) => Ok(false),
        // A failed policy/database read is not proof that an intent is obsolete.
        // Fail the entire page so clients retain their uncertain commands.
        Err(error) => Err(error),
    }
}
impl Store {
    pub async fn pending_commands(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Option<Uuid>,
        after: Option<Uuid>,
        limit: i32,
    ) -> KabiPayResult<OrganizationDocumentPage> {
        limit_page(after, limit)?;
        let tx = self.db.begin().await?;
        let p = current(&tx, actor).await?;
        source_member(&tx, actor, org).await?;
        let rows=tx.query_all(sql("SELECT i.command_id AS id,i.operation,i.membership_id,i.employee_id,i.authority_kind,i.authority_id,i.authority_version,jsonb_build_object('id',i.command_id,'command_id',i.command_id,'resource_id',i.resource_id,'membership_id',i.membership_id,'tenant_id',i.tenant_id,'operation',i.operation,'status',i.status,'expected_version',i.expected_version,'created_at',i.created_at,'updated_at',i.updated_at) AS data FROM kabipay_ops.organization_command_intent i WHERE i.organization_id=$1 AND i.principal_id=$2 AND i.source_tenant_id=$3 AND i.source_user_id=$4 AND i.status IN ('SUBMITTED','AWAITING_CONFIRMATION') AND (i.operation NOT IN ('install_membership','install_grant','install_relationship') OR (EXISTS(SELECT 1 FROM kabipay_ops.organization_company cm WHERE cm.id=i.membership_id AND cm.version=i.membership_version) AND (i.operation<>'install_relationship' OR EXISTS(SELECT 1 FROM kabipay_ops.organization_relationship ar WHERE ar.id=i.resource_id AND ar.version=i.expected_version)) AND (i.operation<>'install_grant' OR EXISTS(SELECT 1 FROM kabipay_ops.principal_role_binding pb WHERE pb.id=i.resource_id AND pb.version=i.expected_version)))) AND ($5::uuid IS NULL OR i.membership_id=$5) AND ($6::uuid IS NULL OR i.command_id>$6) ORDER BY i.command_id LIMIT 501",vec![org.into(),p.id.into(),actor.tenant().into(),actor.user().into(),member.into(),after.into()])).await?;
        let mut data = Vec::new();
        let mut cursor = None;
        let mut more = false;
        for r in &rows {
            let id: Uuid = r.try_get("", "id")?;
            cursor = Some(id);
            let operation: OrganizationOperation =
                serde_json::from_value(Value::String(r.try_get("", "operation")?))?;
            let current_authority = authority(
                &tx,
                actor,
                p.id,
                org,
                r.try_get("", "membership_id")?,
                operation,
                r.try_get("", "employee_id")?,
            )
            .await;
            // A newly granted capability cannot disclose a receipt recorded
            // under a different authority. Omit those obsolete references so
            // recovery converges to a fresh authorized source read.
            if !recoverable_authority(
                current_authority,
                &r.try_get::<String>("", "authority_kind")?,
                r.try_get("", "authority_id")?,
                r.try_get("", "authority_version")?,
            )? {
                continue;
            }
            if data.len() == limit as usize {
                more = true;
                break;
            }
            data.push(r.try_get::<Value>("", "data")?);
        }
        more |= rows.len() == 501;
        let version = graph_version(&tx, org).await?;
        tx.commit().await?;
        let end = if more {
            if data.len() == limit as usize {
                data.last()
                    .and_then(|v| v["id"].as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            } else {
                cursor
            }
        } else {
            None
        };
        Ok(OrganizationDocumentPage {
            schema_version: 1,
            items: async_graphql::Json(data),
            end_cursor: end,
            has_next_page: more,
            graph_version: version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_requires_the_original_authority_kind_id_and_revision() {
        let id = Uuid::new_v4();
        let current = Authority {
            kind: AuthorityKind::ManagementAssignment,
            id,
            version: 2,
            scope: "TEAM".into(),
        };
        assert!(same_authority(&current, "management_assignment", id, 2));
        assert!(!same_authority(&current, "management_assignment", id, 1));
        assert!(!same_authority(&current, "organization_grant", id, 2));
        assert!(!same_authority(
            &current,
            "management_assignment",
            Uuid::new_v4(),
            2
        ));
        assert!(!same_authority(&current, "unknown", id, 2));
    }

    #[test]
    fn authority_failure_cannot_be_reported_as_an_empty_recovery_snapshot() {
        let id = Uuid::new_v4();
        assert!(!recoverable_authority(
            Err(KabiPayError::Forbidden("revoked".into())),
            "organization_grant",
            id,
            1,
        )
        .unwrap());
        assert!(recoverable_authority(
            Err(KabiPayError::Internal("policy read unavailable".into())),
            "organization_grant",
            id,
            1,
        )
        .is_err());
    }
}
