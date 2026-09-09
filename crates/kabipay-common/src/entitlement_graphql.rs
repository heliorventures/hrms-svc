//! Enforce the owning module before business field resolution, including direct subgraph requests.
use std::sync::Arc;
use async_graphql::{extensions::{Extension, ExtensionContext, ExtensionFactory, NextResolve, ResolveInfo}, ServerResult, Value};
use sea_orm::entity::prelude::async_trait;
use crate::{context::ClientClaims, entitlements::Entitlements, subgraph::TenantId, KabiPayError};

#[derive(Clone, Copy)]
pub struct ModuleEntitlement(pub &'static str);

impl ExtensionFactory for ModuleEntitlement {
    fn create(&self) -> Arc<dyn Extension> { Arc::new(*self) }
}

#[async_trait::async_trait]
impl Extension for ModuleEntitlement {
    async fn resolve(&self, ctx: &ExtensionContext<'_>, info: ResolveInfo<'_>, next: NextResolve<'_>) -> ServerResult<Option<Value>> {
        // Federation SDL and GraphQL introspection contain no tenant business data.
        let discovery = info.is_for_introspection || info.name == "__typename"
            || (matches!(info.name, "__schema" | "__type") && info.path_node.parent.is_none())
            || (info.name == "_service" && info.path_node.parent.is_none())
            || info.parent_type == "_Service";
        if !discovery {
            let check = || {
                let claims = ctx.data_opt::<ClientClaims>().ok_or(KabiPayError::Unauthorised)?;
                let tenant = ctx.data_opt::<TenantId>().ok_or(KabiPayError::Unauthorised)?;
                let state = ctx.data_opt::<Entitlements>().ok_or_else(|| KabiPayError::Internal("request entitlement snapshot missing".into()))?;
                state.require_tenant(tenant.0)?;
                state.require_tenant(claims.tenant_id)?;
                state.require(self.0)
            };
            check().map_err(|error| error.into_graphql().into_server_error(info.field.name.pos))?;
        }
        next.run(ctx, info).await
    }
}
