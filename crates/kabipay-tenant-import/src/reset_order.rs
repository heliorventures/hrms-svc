//! Preserve every populated FK dependency; clear a nullable link only to resolve a cycle.
use crate::reset::{deletion_order, ClearLink};
use anyhow::{bail, Result};
use std::collections::HashSet;

pub struct Dependency {
    pub child: String,
    pub parent: String,
    pub clear: Option<ClearLink>,
}

fn reaches(edges: &[Dependency], start: &str, target: &str) -> bool {
    let mut pending = vec![start];
    let mut seen = HashSet::new();
    while let Some(node) = pending.pop() {
        if node == target {
            return true;
        }
        if seen.insert(node) {
            pending.extend(
                edges
                    .iter()
                    .filter(|edge| edge.child == node)
                    .map(|edge| edge.parent.as_str()),
            );
        }
    }
    false
}

pub fn plan(
    tables: &[String],
    mut edges: Vec<Dependency>,
) -> Result<(Vec<String>, Vec<ClearLink>)> {
    // A single DELETE handles self references atomically. Preserved-row references are checked separately.
    edges.retain(|edge| edge.child != edge.parent);
    let mut clears = Vec::new();
    loop {
        let pairs: Vec<_> = edges
            .iter()
            .map(|edge| (edge.child.clone(), edge.parent.clone()))
            .collect();
        if let Ok(order) = deletion_order(tables, &pairs) {
            return Ok((order, clears));
        }
        let Some(index) = edges
            .iter()
            .position(|edge| edge.clear.is_some() && reaches(&edges, &edge.parent, &edge.child))
        else {
            bail!("RESET_FOREIGN_KEY_CYCLE_REQUIRES_REVIEW");
        };
        if let Some(clear) = edges.remove(index).clear {
            clears.push(clear);
        }
    }
}
