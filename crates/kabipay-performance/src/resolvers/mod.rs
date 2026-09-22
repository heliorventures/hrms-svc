pub mod query;
pub mod types;

pub mod administration;
mod administration_cycles;
mod administration_history;
pub mod administration_types;
pub mod administration_pagination;
mod administration_policy;
mod administration_population;

pub use query::QueryRoot as PerformanceQueryRoot;

pub mod mutation;
pub use mutation::MutationRoot as PerformanceMutationRoot;

#[derive(async_graphql::MergedObject)]
pub struct QueryRoot(PerformanceQueryRoot, administration::AdministrationQueryRoot);

impl Default for QueryRoot {
    fn default() -> Self {
        Self(PerformanceQueryRoot, administration::AdministrationQueryRoot)
    }
}

#[derive(async_graphql::MergedObject)]
pub struct MutationRoot(PerformanceMutationRoot, administration::AdministrationMutationRoot);

impl Default for MutationRoot {
    fn default() -> Self {
        Self(PerformanceMutationRoot, administration::AdministrationMutationRoot)
    }
}

#[cfg(test)]
mod concurrency_tests;
