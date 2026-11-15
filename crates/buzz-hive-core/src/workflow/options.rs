//! A workflow's board behavior: which stages hold a card, where a finished card goes.

use crate::workflow::load::Package;

/// The row a person's changes are stored in. `paused_stages` of `None` means the package's
/// own `pause: true` stages stand.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowOptions {
    #[serde(default)]
    pub paused_stages: Option<Vec<String>>,
    #[serde(default)]
    pub next_workflow: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// A workflow's board behavior after the row-or-package fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedOptions {
    /// The stages a card waits before until a person starts it, in stage order.
    pub paused: Vec<String>,
    /// The workflow a finished card hands off to, empty for none.
    pub next: String,
    /// The labels a person added that suggest the workflow.
    pub labels: Vec<String>,
}

/// `package`'s board behavior with `row`'s changes on top. No row, or a row with no pause
/// list, keeps the stages the package marks `pause: true`. A row's list replaces the
/// package's, so an empty list means no pauses.
pub fn resolve_options(package: &Package, row: Option<&WorkflowOptions>) -> ResolvedOptions {
    let chosen: Option<&Vec<String>> = row.and_then(|r| r.paused_stages.as_ref());
    let paused = package
        .workflow
        .stages
        .iter()
        .filter(|stage| match chosen {
            Some(list) => list.contains(&stage.id),
            None => stage.pause,
        })
        .map(|stage| stage.id.clone())
        .collect();
    ResolvedOptions {
        paused,
        next: row.map(|r| r.next_workflow.clone()).unwrap_or_default(),
        labels: row.map(|r| r.labels.clone()).unwrap_or_default(),
    }
}
