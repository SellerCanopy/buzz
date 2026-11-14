//! The fields each stage kind owns, checked per kind.
//!
//! Ported from the `Check` method of each Go stage kind. Paths in the findings are
//! relative to the stage, such as `prompt`.

use crate::domain::workflow::{
    Finding, Stage, KIND_ACTION, KIND_APPROVAL, KIND_PROMPT, KIND_WAIT, WAIT_PULL_REQUEST_STATES,
};
use crate::workflow::actions::check_action;
use crate::workflow::names::{artifact_name, ARTIFACT_NAME_RULE};

const REPEAT_UNTIL_PLAN_COMPLETE: &str = "plan_complete";
const REPEAT_UNTIL_AGENT_DONE: &str = "agent_done";

/// The findings for the fields the stage's kind owns. A kind that is missing or unknown
/// has none: the stage check reports it.
pub fn check_kind(stage: &Stage) -> Vec<Finding> {
    match stage.kind.as_str() {
        KIND_PROMPT => check_prompt(stage),
        KIND_APPROVAL => check_approval(stage),
        KIND_WAIT => check_wait(stage),
        KIND_ACTION => check_action(stage),
        _ => Vec::new(),
    }
}

/// A prompt stage with no prompt, and a repeat group this build cannot run.
fn check_prompt(stage: &Stage) -> Vec<Finding> {
    let mut out = Vec::new();
    if stage.prompt.is_empty() && stage.prompt_file.is_empty() && stage.command.is_empty() {
        out.push(Finding::error(
            "prompt",
            "prompt_missing",
            "a prompt stage needs prompt, prompt_file or command",
        ));
    }
    if let Some(repeat) = &stage.repeat {
        match repeat.until.as_str() {
            REPEAT_UNTIL_PLAN_COMPLETE => {
                if repeat.plan.is_empty() {
                    out.push(Finding::error(
                        "repeat.plan",
                        "repeat_plan_missing",
                        format!(
                            "repeat until {} needs plan, the artifact whose checkboxes it counts",
                            repeat.until
                        ),
                    ));
                } else if !artifact_name(&repeat.plan) {
                    out.push(Finding::error(
                        "repeat.plan",
                        "artifact_name_invalid",
                        format!(
                            "artifact name {:?} must match {ARTIFACT_NAME_RULE}",
                            repeat.plan
                        ),
                    ));
                }
            }
            REPEAT_UNTIL_AGENT_DONE => {}
            other => out.push(Finding::error(
                "repeat.until",
                "repeat_until_unknown",
                format!(
                    "repeat until {other:?} is not a repeat mode; want {REPEAT_UNTIL_PLAN_COMPLETE} or {REPEAT_UNTIL_AGENT_DONE}"
                ),
            )),
        }
        if repeat.max_rounds < 0 {
            out.push(Finding::error(
                "repeat.max_rounds",
                "count_negative",
                "repeat.max_rounds must be 0 or more",
            ));
        }
        if repeat.stall_after < 0 {
            out.push(Finding::error(
                "repeat.stall_after",
                "count_negative",
                "repeat.stall_after must be 0 or more",
            ));
        }
    }
    out
}

/// An approval stage that names no artifact, or a name that is not an artifact name.
fn check_approval(stage: &Stage) -> Vec<Finding> {
    let artifacts = stage
        .approval
        .as_ref()
        .map(|a| a.artifacts.as_slice())
        .unwrap_or(&[]);
    if artifacts.is_empty() {
        return vec![Finding::error(
            "approval.artifacts",
            "approval_artifacts_missing",
            "an approval stage names the artifacts a person approves in approval.artifacts",
        )];
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, name) in artifacts.iter().enumerate() {
        let path = format!("approval.artifacts[{i}]");
        if !artifact_name(name) {
            out.push(Finding::error(
                path,
                "artifact_name_invalid",
                format!("artifact name {name:?} must match {ARTIFACT_NAME_RULE}"),
            ));
        } else if seen.contains(name) {
            out.push(Finding::error(
                path,
                "approval_artifact_duplicate",
                format!("artifact {name} is listed more than once"),
            ));
        }
        seen.insert(name);
    }
    out
}

/// A wait stage with nothing to wait for, and a pull request state it cannot wait for.
fn check_wait(stage: &Stage) -> Vec<Finding> {
    let Some(wait) = stage
        .wait
        .as_ref()
        .filter(|w| !(w.labels.is_empty() && w.pull_request.is_empty()))
    else {
        return vec![Finding::error(
            "wait.labels",
            "wait_labels_missing",
            "a wait stage names the labels it waits for in wait.labels, or a pull request state in wait.pull_request",
        )];
    };
    let mut out = Vec::new();
    for (i, label) in wait.labels.iter().enumerate() {
        if label.trim().is_empty() {
            out.push(Finding::error(
                format!("wait.labels[{i}]"),
                "label_empty",
                "a label has no name",
            ));
        }
    }
    let state = &wait.pull_request;
    if !state.is_empty() && !WAIT_PULL_REQUEST_STATES.contains(&state.as_str()) {
        out.push(Finding::error(
            "wait.pull_request",
            "wait_pull_request_unknown",
            format!(
                "wait.pull_request {state:?} is not a state to wait for; want {}",
                WAIT_PULL_REQUEST_STATES.join(" or ")
            ),
        ));
    }
    if wait.poll.nanos() < 0 {
        out.push(Finding::error(
            "wait.poll",
            "duration_negative",
            "wait.poll must be zero or more",
        ));
    }
    if wait.timeout.nanos() < 0 {
        out.push(Finding::error(
            "wait.timeout",
            "duration_negative",
            "wait.timeout must be zero or more",
        ));
    }
    out
}
