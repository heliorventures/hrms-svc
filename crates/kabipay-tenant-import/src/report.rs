//! Masked section outcomes describe committed facts, never credentials or raw DB errors.
use crate::contract::SourceRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionOutcome {
    pub source_ref: SourceRef,
    pub section: String,
    pub outcome: String,
    pub code: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportReport {
    pub run_id: uuid::Uuid,
    pub tenant_id: uuid::Uuid,
    pub committed: bool,
    pub sections: Vec<SectionOutcome>,
    #[serde(default)]
    pub issues: Vec<SourceIssue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceIssue {
    pub source_ref: Option<SourceRef>,
    pub severity: String,
    pub section: String,
    pub code: String,
    pub field: String,
}
impl ImportReport {
    pub fn count(&self, outcome: &str) -> usize {
        self.sections
            .iter()
            .filter(|item| item.outcome == outcome)
            .count()
    }
    pub fn record(&mut self, source_ref: &SourceRef, section: &str, outcome: &str, code: &str) {
        self.sections.push(SectionOutcome {
            source_ref: source_ref.clone(),
            section: section.into(),
            outcome: outcome.into(),
            code: code.into(),
        });
    }
}
