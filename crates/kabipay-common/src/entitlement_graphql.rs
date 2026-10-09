//! Enforce the owning module before business field resolution, including direct subgraph requests.
use std::sync::Arc;
use async_graphql::{extensions::{Extension, ExtensionContext, ExtensionFactory, NextResolve, ResolveInfo}, ServerResult, Value};
use sea_orm::entity::prelude::async_trait;
use crate::{context::ClientClaims, entitlements::Entitlements, subgraph::TenantId, KabiPayError};

#[derive(Clone, Copy)]
pub struct ModuleEntitlement(pub &'static str);

/// The Payroll host contains separately entitled modules. Match actual field/type names,
/// never aliases, document text or a client supplied module. Unknown fields keep the host gate.
fn owning_module(host:&'static str,parent:&str,field:&str,root:bool)->&'static str {
    const LOAN_FIELDS:&[&str]=&["myLoans","loanAccounts","myLoanRequests","loanRequestQueue","loanAccount","loanPolicies","loanPolicyVersions","loanLedger","loanPayments","loanSchedules","previewLoanReversal","publishLoanPolicy","retireLoanPolicy","saveLoanRequest","submitLoanRequest","decideLoanRequest","withdrawLoanRequest","recordLoanDisbursement","recordLoanReceipt","setLoanDeduction","setLoanPeriodOverride","reverseLoanPosting"];
    const LOAN_TYPES:&[&str]=&["LoanAccount","LoanRequest","LoanPolicy","LoanPosting","LoanPayment","LoanSchedule","LoanCommandResult","LoanCorrectionPreview","LoanPageInfo","LoanAccountConnection","LoanRequestConnection","LoanPolicyConnection","LoanPostingConnection","LoanPaymentConnection","LoanScheduleConnection"];
    if host=="PAYROLL" && ((root && LOAN_FIELDS.contains(&field)) || (!root && LOAN_TYPES.contains(&parent))){"LOANS"}else{host}
}

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
                state.require(owning_module(self.0,info.parent_type,info.name,info.path_node.parent.is_none()))
            };
            check().map_err(|error| error.into_graphql().into_server_error(info.field.name.pos))?;
        }
        next.run(ctx, info).await
    }
}
