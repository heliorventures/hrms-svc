use crate::{
    identity::SourceIdentity,
    models::{Change, ChangeResult, Organization, RoleCapability},
    queries::{OrganizationConfigurationKind, OrganizationConfigurationPage, OrganizationIdentity},
    store::Store,
};
use async_graphql::{Context, EmptySubscription, MergedObject, Object, Schema};
use kabipay_common::{KabiPayError, KabiPayResult};
use uuid::Uuid;
#[derive(Default)]
pub struct MetadataQuery;
#[derive(Default)]
pub struct MetadataMutation;
#[derive(Default, MergedObject)]
pub struct QueryRoot(MetadataQuery, crate::workspace_graphql::WorkspaceQuery);
#[derive(Default, MergedObject)]
pub struct MutationRoot(
    MetadataMutation,
    crate::workspace_graphql::WorkspaceMutation,
);
pub type OrganizationSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;
pub(crate) fn actor<'a>(ctx: &'a Context<'_>) -> KabiPayResult<&'a SourceIdentity> {
    ctx.data_opt().ok_or(KabiPayError::Unauthorised)
}
pub(crate) fn store<'a>(ctx: &'a Context<'_>) -> KabiPayResult<&'a Store> {
    ctx.data_opt()
        .ok_or_else(|| KabiPayError::Internal("organization store unavailable".into()))
}
pub(crate) fn gql<T>(result: KabiPayResult<T>) -> async_graphql::Result<T> {
    result.map_err(KabiPayError::into_graphql)
}
#[Object]
impl MetadataQuery {
    async fn my_organizations(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<Organization>> {
        let actor = gql(actor(ctx))?;
        gql(gql(store(ctx))?.organizations(actor).await)
    }
    async fn my_organization_identity(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<OrganizationIdentity> {
        let actor = gql(actor(ctx))?;
        gql(gql(store(ctx))?.identity(actor).await)
    }
    async fn organization_capability_catalog(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<String>> {
        gql(actor(ctx))?;
        Ok(crate::policy::CATALOG.iter().map(|c| (*c).into()).collect())
    }
    async fn organization_configuration(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        kind: OrganizationConfigurationKind,
        after: Option<Uuid>,
        first: Option<i32>,
    ) -> async_graphql::Result<OrganizationConfigurationPage> {
        let actor = gql(actor(ctx))?;
        gql(gql(store(ctx))?
            .configuration(actor, organization_id, kind, after, first.unwrap_or(50))
            .await)
    }
}
async fn apply(
    ctx: &Context<'_>,
    org: Uuid,
    version: i64,
    change: Change,
) -> async_graphql::Result<ChangeResult> {
    let actor = gql(actor(ctx))?;
    gql(gql(store(ctx))?.apply(actor, org, version, change).await)
}
#[Object]
impl MetadataMutation {
    async fn register_organization_identity(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Uuid> {
        let actor = gql(actor(ctx))?;
        gql(gql(store(ctx))?.register(actor).await)
    }
    async fn revoke_my_organization_identity(
        &self,
        ctx: &Context<'_>,
        expected_link_version: i64,
    ) -> async_graphql::Result<Uuid> {
        let actor = gql(actor(ctx))?;
        gql(gql(store(ctx))?
            .revoke_own_identity(actor, expected_link_version)
            .await)
    }
    async fn invite_organization_company(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        tenant_id: Uuid,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::InviteCompany { tenant: tenant_id },
        )
        .await
    }
    async fn accept_organization_company(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        membership_id: Uuid,
        expected_membership_version: i64,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::AcceptCompany {
                membership: membership_id,
                version: expected_membership_version,
            },
        )
        .await
    }
    async fn suspend_organization_company(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        membership_id: Uuid,
        expected_membership_version: i64,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::SuspendCompany {
                membership: membership_id,
                version: expected_membership_version,
            },
        )
        .await
    }
    async fn resume_organization_company(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        membership_id: Uuid,
        expected_membership_version: i64,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::ResumeCompany {
                membership: membership_id,
                version: expected_membership_version,
            },
        )
        .await
    }
    async fn begin_organization_company_detach(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        membership_id: Uuid,
        expected_membership_version: i64,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::BeginDetachCompany {
                membership: membership_id,
                version: expected_membership_version,
            },
        )
        .await
    }
    async fn create_organization_role(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        name: String,
        capabilities: Vec<RoleCapability>,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::CreateRole { name, capabilities },
        )
        .await
    }
    async fn replace_organization_role_capabilities(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        role_id: Uuid,
        expected_role_version: i64,
        capabilities: Vec<RoleCapability>,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::ReplaceRole {
                role: role_id,
                version: expected_role_version,
                capabilities,
            },
        )
        .await
    }
    async fn grant_organization_role(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        principal_id: Uuid,
        role_id: Uuid,
        company_membership_ids: Vec<Uuid>,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::GrantRole {
                principal: principal_id,
                role: role_id,
                companies: company_membership_ids,
            },
        )
        .await
    }
    async fn revoke_organization_role_binding(
        &self,
        ctx: &Context<'_>,
        organization_id: Uuid,
        expected_version: i64,
        binding_id: Uuid,
        expected_binding_version: i64,
    ) -> async_graphql::Result<ChangeResult> {
        apply(
            ctx,
            organization_id,
            expected_version,
            Change::RevokeBinding {
                binding: binding_id,
                version: expected_binding_version,
            },
        )
        .await
    }
}
pub fn build_schema(store: Store) -> OrganizationSchema {
    schema_builder(store).finish()
}
pub fn schema_builder(
    store: Store,
) -> async_graphql::SchemaBuilder<QueryRoot, MutationRoot, EmptySubscription> {
    Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        EmptySubscription,
    )
    .limit_depth(12)
    .limit_complexity(1000)
    .data(store)
}
