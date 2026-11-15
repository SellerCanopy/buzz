//! A stage's conditions, checked before the stage opens a thread.

use crate::domain::workflow::{Conditions, Stage};

/// Whether `have` holds every label in `want`.
pub fn labels_met(have: &[String], want: &[String]) -> bool {
    want.iter().all(|label| have.contains(label))
}

/// The stage's conditions when the card does not meet them. A stage with no conditions, or
/// with every label present, returns `None` and runs.
pub fn unmet<'a>(stage: &'a Stage, labels: &[String]) -> Option<&'a Conditions> {
    stage
        .conditions
        .as_ref()
        .filter(|c| !labels_met(labels, &c.labels))
}

/// How an unmet condition reads in the log and on the card: `labels a, b`.
pub fn describe(conditions: &Conditions) -> String {
    format!("labels {}", conditions.labels.join(", "))
}
