//! Validation of an authoritative primary-management graph snapshot.
use crate::{EffectivePeriod, EmploymentId, OrganizationError, PrincipalId};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct PrimaryReportingEdge {
    pub subject_employment_id: EmploymentId,
    pub manager_employment_id: EmploymentId,
    pub subject_principal_id: PrincipalId,
    pub manager_principal_id: PrincipalId,
    pub period: EffectivePeriod,
}
/// Validate a consistent, organization-authorized graph snapshot at a specified instant.
/// The relationship writer must serialize/version graph edits and validate each affected
/// future boundary as well; this pure function does not fetch or authorize a graph.
///
/// # Errors
/// Rejects self-reporting, conflicting identities, multiple primary managers and cycles.
pub fn validate_primary_reporting_graph(
    edges: &[PrimaryReportingEdge],
    at: DateTime<Utc>,
) -> Result<(), OrganizationError> {
    validate_graph(edges, at, false)
}
/// Validate the union of old and reserved edges while company installations are
/// incomplete. Multiple potential managers are allowed; cycles are not.
///
/// # Errors
/// Rejects self reporting, inconsistent identities and any possible reporting cycle.
pub fn validate_reporting_transition_graph(
    edges: &[PrimaryReportingEdge],
    at: DateTime<Utc>,
) -> Result<(), OrganizationError> {
    validate_graph(edges, at, true)
}
fn validate_graph(
    edges: &[PrimaryReportingEdge],
    at: DateTime<Utc>,
    transition: bool,
) -> Result<(), OrganizationError> {
    let mut managers = HashMap::new();
    let mut people = HashMap::new();
    let mut person_managers: HashMap<PrincipalId, HashSet<PrincipalId>> = HashMap::new();
    for edge in edges.iter().filter(|edge| edge.period.contains(at)) {
        if edge.subject_employment_id == edge.manager_employment_id
            || edge.subject_principal_id == edge.manager_principal_id
        {
            return Err(OrganizationError::SelfReporting);
        }
        for (employment, person) in [
            (edge.subject_employment_id, edge.subject_principal_id),
            (edge.manager_employment_id, edge.manager_principal_id),
        ] {
            if people
                .insert(employment, person)
                .is_some_and(|previous| previous != person)
            {
                return Err(OrganizationError::InconsistentEmploymentIdentity);
            }
        }
        if managers
            .insert(edge.subject_employment_id, edge.manager_employment_id)
            .is_some()
            && !transition
        {
            return Err(OrganizationError::MultiplePrimaryManagers);
        }
        person_managers
            .entry(edge.subject_principal_id)
            .or_default()
            .insert(edge.manager_principal_id);
    }
    let mut finished = HashSet::new();
    for &start in managers.keys() {
        let mut path = HashSet::new();
        let mut current = start;
        while let Some(&next) = managers.get(&current) {
            if finished.contains(&current) {
                break;
            }
            if !path.insert(current) {
                return Err(OrganizationError::ReportingCycle);
            }
            current = next;
        }
        finished.extend(path);
    }
    // Employment rows can differ while referring to the same person. Validate
    // the person graph as well, using an iterative topological traversal.
    let mut incoming: HashMap<PrincipalId, usize> = HashMap::new();
    for (&subject, managers) in &person_managers {
        incoming.entry(subject).or_default();
        for &manager in managers {
            *incoming.entry(manager).or_default() += 1;
        }
    }
    let mut ready: Vec<_> = incoming
        .iter()
        .filter_map(|(&p, &n)| (n == 0).then_some(p))
        .collect();
    let mut visited = 0;
    while let Some(person) = ready.pop() {
        visited += 1;
        if let Some(managers) = person_managers.get(&person) {
            for manager in managers {
                if let Some(count) = incoming.get_mut(manager) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push(*manager);
                    }
                }
            }
        }
    }
    if visited != incoming.len() {
        return Err(OrganizationError::ReportingCycle);
    }
    Ok(())
}
