use crate::relationships::RelationshipProposal;
use crate::{
    execution::Execution,
    graphql::{actor, gql, store},
    workspace::*,
};
use async_graphql::{Context, InputObject, Json, Object, SimpleObject};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{
    OrganizationCommand, OrganizationCommandOutcome, OrganizationOperation,
};
use serde_json::Value;
use uuid::Uuid;
#[derive(Default)]
pub struct WorkspaceQuery;
#[derive(Default)]
pub struct WorkspaceMutation;
#[derive(InputObject)]
pub struct OrganizationCommandInput {
    pub command_id: Uuid,
    pub resource_id: Uuid,
    pub expected_version: i64,
    pub operation: OrganizationOperation,
    pub payload: Json<Value>,
}
impl From<OrganizationCommandInput> for OrganizationCommand {
    fn from(c: OrganizationCommandInput) -> Self {
        Self {
            command_id: c.command_id,
            resource_id: c.resource_id,
            expected_version: c.expected_version,
            operation: c.operation,
            payload: c.payload.0,
        }
    }
}
#[derive(SimpleObject)]
pub struct CommandOutcome {
    pub command_id: Uuid,
    pub resource_id: Uuid,
    pub status: String,
    pub resource_version: i64,
    pub result: Json<Value>,
}
impl From<OrganizationCommandOutcome> for CommandOutcome {
    fn from(c: OrganizationCommandOutcome) -> Self {
        Self {
            command_id: c.command_id,
            resource_id: c.resource_id,
            status: c.status,
            resource_version: c.resource_version,
            result: Json(c.result),
        }
    }
}
fn execution<'a>(ctx: &'a Context<'_>) -> KabiPayResult<&'a Execution> {
    ctx.data_opt()
        .ok_or_else(|| KabiPayError::Internal("organization execution is not configured".into()))
}
#[Object]
impl WorkspaceQuery {
    #[allow(clippy::too_many_arguments)] // Explicit bounded audit range and cursor.
    async fn organization_audit_history(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(execution(ctx))?
            .audit_history(
                gql(actor(ctx))?,
                organization_id,
                from,
                to,
                after,
                first.unwrap_or(50),
            )
            .await)
        .map(Json)
    }

    async fn organization_pending_commands(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Option<Uuid>,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?
            .pending_commands(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
    async fn organization_workspaces(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<OrganizationWorkspace>> {
        gql(gql(store(ctx))?.workspaces(gql(actor(ctx))?).await)
    }
    async fn organization_companies(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
    ) -> async_graphql::Result<Vec<OrganizationCompany>> {
        gql(gql(store(ctx))?
            .companies(gql(actor(ctx))?, organization_id)
            .await)
    }
    async fn organization_invitations(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?.invitations(gql(actor(ctx))?).await)
    }
    async fn organization_directory(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?
            .directory(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
    async fn organization_relationships(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?
            .relationships(
                gql(actor(ctx))?,
                organization_id,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
    async fn organization_approvals(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Option<Uuid>,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?
            .approvals(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
    async fn organization_team(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Option<Uuid>,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(store(ctx))?
            .team(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
    #[allow(clippy::too_many_arguments)] // Mirrors the approved flat GraphQL command contract.
    async fn organization_resource(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        operation: OrganizationOperation,
        resource_id: Uuid,
        expected_version: i64,
        payload: Option<Json<Value>>,
    ) -> async_graphql::Result<CommandOutcome> {
        if !operation.is_read()
            || matches!(
                operation,
                OrganizationOperation::Directory | OrganizationOperation::ReadReceipt
            )
        {
            return Err(KabiPayError::Validation(
                "use the dedicated directory or command-status operation".into(),
            )
            .into_graphql());
        }
        gql(gql(execution(ctx))?
            .execute(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                OrganizationCommand {
                    command_id: Uuid::new_v4(),
                    resource_id,
                    expected_version,
                    operation,
                    payload: payload
                        .map(|v| v.0)
                        .unwrap_or_else(|| serde_json::json!({})),
                },
            )
            .await)
        .map(Into::into)
    }
    async fn organization_command_status(
        &self,
        ctx: &Context<'_>,
        command_id: Uuid,
    ) -> async_graphql::Result<CommandOutcome> {
        gql(gql(execution(ctx))?
            .status(gql(actor(ctx))?, command_id)
            .await)
        .map(Into::into)
    }
    async fn organization_progress(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(execution(ctx))?
            .progress(gql(actor(ctx))?, organization_id, membership_ids, from, to)
            .await)
        .map(Json)
    }
    async fn organization_operational_progress(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(execution(ctx))?
            .operational_progress(gql(actor(ctx))?, organization_id, membership_ids, from, to)
            .await)
        .map(Json)
    }
    async fn organization_history_archive(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        archive_id: Uuid,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(execution(ctx))?
            .history_archive(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                archive_id,
                None,
            )
            .await)
        .map(Json)
    }
    #[allow(clippy::too_many_arguments)] // Range, company scope and cursor are explicit GraphQL fields.
    async fn organization_history(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(execution(ctx))?
            .history(
                gql(actor(ctx))?,
                organization_id,
                membership_ids,
                from,
                to,
                after,
                first.unwrap_or(50),
            )
            .await)
    }
}
#[Object]
impl WorkspaceMutation {
    async fn export_organization_history_archive(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        archive_id: Uuid,
        from: DateTime<Utc>,
        through: DateTime<Utc>,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(execution(ctx))?
            .history_archive(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                archive_id,
                Some((from, through)),
            )
            .await)
        .map(Json)
    }
    async fn cancel_pending_organization_command(
        &self,
        ctx: &Context<'_>,
        command_id: Uuid,
    ) -> async_graphql::Result<CommandOutcome> {
        gql(gql(execution(ctx))?
            .cancel_pending(gql(actor(ctx))?, command_id)
            .await)
        .map(Into::into)
    }

    async fn install_organization_membership(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        expected_version: i64,
        command_id: Uuid,
    ) -> async_graphql::Result<CommandOutcome> {
        gql(gql(execution(ctx))?
            .install_membership(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                expected_version,
                command_id,
            )
            .await)
        .map(Into::into)
    }

    async fn execute_organization_command(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        command: OrganizationCommandInput,
    ) -> async_graphql::Result<CommandOutcome> {
        if command.operation.is_read()
            || matches!(
                command.operation,
                OrganizationOperation::CancelCommand
                    | OrganizationOperation::InstallMembership
                    | OrganizationOperation::InstallGrant
                    | OrganizationOperation::InstallRelationship
            )
        {
            return Err(
                KabiPayError::Validation("use the dedicated operation".into()).into_graphql(),
            );
        }
        gql(gql(execution(ctx))?
            .execute(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                command.into(),
            )
            .await)
        .map(Into::into)
    }
    async fn install_organization_grant(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        binding_id: Uuid,
        expected_version: i64,
        command_id: Uuid,
    ) -> async_graphql::Result<CommandOutcome> {
        gql(gql(execution(ctx))?
            .execute(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                OrganizationCommand {
                    command_id,
                    resource_id: binding_id,
                    expected_version,
                    operation: OrganizationOperation::InstallGrant,
                    payload: serde_json::json!({}),
                },
            )
            .await)
        .map(Into::into)
    }
    async fn sync_organization_directory(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationDocumentPage> {
        gql(gql(execution(ctx))?
            .sync_directory(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                after,
                first.unwrap_or(100),
            )
            .await)
    }
    #[allow(clippy::too_many_arguments)] // Preserves the explicit reviewed relationship mutation fields.
    async fn propose_organization_relationship(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_graph_version: i64,
        subject_employment_ref_id: Uuid,
        manager_employment_ref_id: Uuid,
        relationship_type: String,
        effective_from: DateTime<Utc>,
        effective_to: Option<DateTime<Utc>>,
        capabilities: Vec<String>,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(store(ctx))?
            .propose_relationship(
                gql(actor(ctx))?,
                RelationshipProposal {
                    organization_id,
                    expected_graph_version,
                    subject_employment_ref_id,
                    manager_employment_ref_id,
                    relationship_type,
                    effective_from,
                    effective_to,
                    capabilities,
                },
            )
            .await)
        .map(Json)
    }
    #[allow(clippy::too_many_arguments)] // Explicit revisioned relationship command.
    async fn change_organization_relationship_manager(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        assignment_id: Uuid,
        expected_version: i64,
        expected_graph_version: i64,
        manager_employment_ref_id: Uuid,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(store(ctx))?
            .change_relationship_manager(
                gql(actor(ctx))?,
                organization_id,
                assignment_id,
                expected_version,
                expected_graph_version,
                manager_employment_ref_id,
            )
            .await)
        .map(Json)
    }
    async fn end_organization_relationship(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        assignment_id: Uuid,
        expected_version: i64,
        expected_graph_version: i64,
    ) -> async_graphql::Result<Json<Value>> {
        gql(gql(store(ctx))?
            .end_relationship(
                gql(actor(ctx))?,
                organization_id,
                assignment_id,
                expected_version,
                expected_graph_version,
            )
            .await)
        .map(Json)
    }
    async fn install_organization_relationship(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        membership_id: Uuid,
        assignment_id: Uuid,
        expected_version: i64,
        command_id: Uuid,
    ) -> async_graphql::Result<CommandOutcome> {
        gql(gql(execution(ctx))?
            .execute(
                gql(actor(ctx))?,
                organization_id,
                membership_id,
                OrganizationCommand {
                    command_id,
                    resource_id: assignment_id,
                    expected_version,
                    operation: OrganizationOperation::InstallRelationship,
                    payload: serde_json::json!({}),
                },
            )
            .await)
        .map(Into::into)
    }
}
