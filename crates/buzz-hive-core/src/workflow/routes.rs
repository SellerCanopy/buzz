//! Route targets and the results a stage may report.

use crate::domain::workflow::{Stage, RESULT_NEEDS_WORK, STANDARD_RESULTS};

/// Route targets beside stage ids. `done` is never written in a workflow file: it is where
/// `passed` goes after the last stage. None of the three can be a stage id.
pub const ROUTE_STOP: &str = "stop";
pub const ROUTE_ASK: &str = "ask";
pub const ROUTE_DONE: &str = "done";

/// The standard results followed by the stage's own.
pub fn allowed_results(stage: &Stage) -> Vec<String> {
    let mut out: Vec<String> = STANDARD_RESULTS.iter().map(|r| (*r).to_owned()).collect();
    for result in &stage.results {
        if !out.contains(result) {
            out.push(result.clone());
        }
    }
    out
}

/// The results an agent may report from the stage: the ones [`allowed_results`] lists, less
/// `needs_work` when the stage has no route for it. The workflow decides where every result
/// goes, so the agent is not offered one that would leave the card with nowhere to go. A
/// stage with a `needs_work` route keeps it, and so does a repeat stage, where the agent
/// reports the results that end or continue a round.
pub fn reportable_results(stage: &Stage) -> Vec<String> {
    let mut out = allowed_results(stage);
    let routed = stage
        .on
        .get(RESULT_NEEDS_WORK)
        .is_some_and(|route| !route.to.is_empty());
    if stage.repeat.is_none() && !routed {
        out.retain(|r| r != RESULT_NEEDS_WORK);
    }
    out
}
