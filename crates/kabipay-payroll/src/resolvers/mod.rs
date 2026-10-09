pub mod mutation;
pub mod query;
pub mod types;

#[derive(async_graphql::MergedObject)]
pub struct QueryRoot(pub query::PayrollQueryRoot, pub crate::loans::LoanQuery);
impl Default for QueryRoot {
    fn default() -> Self {
        Self(query::PayrollQueryRoot, crate::loans::LoanQuery)
    }
}
#[derive(async_graphql::MergedObject)]
pub struct MutationRoot(
    pub mutation::PayrollMutationRoot,
    pub crate::loans::LoanMutation,
);
impl Default for MutationRoot {
    fn default() -> Self {
        Self(mutation::PayrollMutationRoot, crate::loans::LoanMutation)
    }
}
