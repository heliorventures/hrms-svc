pub mod mutation;
pub mod query;
pub mod types;

#[derive(async_graphql::MergedObject)]
pub struct QueryRoot(
    pub query::PayrollQueryRoot,
    pub crate::loans::LoanQuery,
    pub crate::fnf::exit_resolvers::ExitReviewQuery,
    pub crate::fnf::resolvers::FnfQuery,
);
impl Default for QueryRoot {
    fn default() -> Self {
        Self(
            query::PayrollQueryRoot,
            crate::loans::LoanQuery,
            crate::fnf::exit_resolvers::ExitReviewQuery,
            crate::fnf::resolvers::FnfQuery,
        )
    }
}
#[derive(async_graphql::MergedObject)]
pub struct MutationRoot(
    pub mutation::PayrollMutationRoot,
    pub crate::loans::LoanMutation,
    pub crate::fnf::exit_resolvers::ExitReviewMutation,
    pub crate::fnf::resolvers::FnfMutation,
);
impl Default for MutationRoot {
    fn default() -> Self {
        Self(
            mutation::PayrollMutationRoot,
            crate::loans::LoanMutation,
            crate::fnf::exit_resolvers::ExitReviewMutation,
            crate::fnf::resolvers::FnfMutation,
        )
    }
}
