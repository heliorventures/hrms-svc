//! Structural admission for anonymous gateway schema composition, never business data.
use async_graphql::{
    parser::types::{
        DocumentOperations, ExecutableDocument, OperationType, Selection, SelectionSet,
    },
    Request,
};
use std::collections::HashSet;

const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_SELECTIONS: usize = 2048;
const MAX_DEPTH: usize = 32;

pub(super) fn is_schema_discovery(request: &mut Request) -> bool {
    if request.query.len() > MAX_QUERY_BYTES {
        return false;
    }
    let operation_name = request.operation_name.clone();
    let Ok(document) = request.parsed_query() else {
        return false;
    };
    // Match async-graphql's operation selection exactly; never choose the first
    // operation of an ambiguous document or interpret an alias as a field name.
    let operation = match (&document.operations, operation_name.as_deref()) {
        (DocumentOperations::Single(operation), None) => Some(operation),
        (DocumentOperations::Multiple(operations), Some(name)) => operations.get(name),
        (DocumentOperations::Multiple(operations), None) if operations.len() == 1 => {
            operations.values().next()
        }
        _ => None,
    };
    let Some(operation) = operation else {
        return false;
    };
    if operation.node.ty != OperationType::Query {
        return false;
    }
    let mut visitor = MetadataSelections {
        document,
        remaining: MAX_SELECTIONS,
        active_fragments: HashSet::new(),
    };
    visitor.check(&operation.node.selection_set.node, true, 0)
}

struct MetadataSelections<'a> {
    document: &'a ExecutableDocument,
    remaining: usize,
    active_fragments: HashSet<&'a str>,
}

impl<'a> MetadataSelections<'a> {
    fn check(&mut self, selection_set: &'a SelectionSet, root: bool, depth: usize) -> bool {
        if depth > MAX_DEPTH || (root && selection_set.items.is_empty()) {
            return false;
        }
        for selection in &selection_set.items {
            let Some(remaining) = self.remaining.checked_sub(1) else {
                return false;
            };
            self.remaining = remaining;
            let allowed = match &selection.node {
                Selection::Field(field) => {
                    (!root
                        || matches!(
                            field.node.name.node.as_str(),
                            "__schema" | "__type" | "__typename" | "_service"
                        ))
                        && self.check(&field.node.selection_set.node, false, depth + 1)
                }
                Selection::InlineFragment(fragment) => {
                    self.check(&fragment.node.selection_set.node, root, depth + 1)
                }
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.as_str();
                    let Some(fragment) = self.document.fragments.get(name) else {
                        return false;
                    };
                    if !self.active_fragments.insert(name) {
                        return false;
                    }
                    let valid = self.check(&fragment.node.selection_set.node, root, depth + 1);
                    self.active_fragments.remove(name);
                    valid
                }
            };
            if !allowed {
                return false;
            }
        }
        true
    }
}
