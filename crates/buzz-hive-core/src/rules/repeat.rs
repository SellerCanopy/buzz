//! A repeat stage: what follows each round. Ported from the Go engine's `rules/decide.go`
//! (`RepeatDecision`, `AgentDoneDecision`).
//!
//! `fresh_session` decides where the next round is posted, which the reducer settles: a fresh
//! round opens a new thread root, and any other round replies in the stage's existing root.

use crate::domain::workflow::{Repeat, RESULT_NEEDS_WORK, RESULT_PASSED};
use crate::workflow::kinds::REPEAT_UNTIL_AGENT_DONE;

/// What a repeat stage does after a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatStep {
    /// Run another round.
    Next,
    /// The work is done: route `passed`.
    Pass,
    /// Route on the reported result as it is.
    Route,
    /// The plan has no checkboxes, so progress cannot be measured. A person decides.
    NoCheckboxes,
    /// The plan has not moved for `stall_after` rounds. A person decides.
    Stalled,
    /// `max_rounds` rounds ran. A person decides.
    MaxRounds,
}

/// The limits used when a stage leaves them at zero.
pub const DEFAULT_MAX_ROUNDS: i64 = 20;
pub const DEFAULT_STALL_AFTER: i64 = 3;

/// What [`repeat_decision`] carries from one round to the next.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepeatState {
    /// Checked items after the previous round, or at the start.
    pub last_checked: u32,
    /// Rounds in a row that left the checked count as it was.
    pub unchanged: u32,
}

/// `value` when it is above zero, otherwise `default`.
pub fn or_default(value: i64, default: i64) -> i64 {
    if value > 0 {
        value
    } else {
        default
    }
}

/// Whether the stage runs until its agent says it is done, not until a plan is complete.
pub fn is_agent_done(spec: &Repeat) -> bool {
    spec.until == REPEAT_UNTIL_AGENT_DONE
}

/// What follows `round` of a repeat stage that counts a plan's checkboxes, from the result the
/// round reported and the plan's `checked` and `total` items. Failed, needs input and custom
/// results route at once. Otherwise completion is measured from the plan: every item checked
/// passes, no items at all, no change for `stall_after` rounds or reaching `max_rounds` need a
/// person, and anything else runs another round. A round that reported no result is measured
/// the same way, so the plan alone decides.
pub fn repeat_decision(
    mut state: RepeatState,
    round: u32,
    result: &str,
    checked: u32,
    total: u32,
    spec: &Repeat,
) -> (RepeatStep, RepeatState) {
    if !matches!(result, "" | RESULT_PASSED | RESULT_NEEDS_WORK) {
        return (RepeatStep::Route, state);
    }
    if total == 0 {
        return (RepeatStep::NoCheckboxes, state);
    }
    if checked == total {
        return (RepeatStep::Pass, state);
    }
    if checked == state.last_checked {
        state.unchanged += 1;
    } else {
        state.unchanged = 0;
    }
    state.last_checked = checked;
    if i64::from(state.unchanged) >= or_default(spec.stall_after, DEFAULT_STALL_AFTER) {
        return (RepeatStep::Stalled, state);
    }
    if i64::from(round) >= or_default(spec.max_rounds, DEFAULT_MAX_ROUNDS) {
        return (RepeatStep::MaxRounds, state);
    }
    (RepeatStep::Next, state)
}

/// What follows `round` of a repeat stage that runs until the agent reports done. `passed` is
/// the done signal. Needs work, or no result, runs another round until `max_rounds`, which
/// needs a person. Failed, needs input and custom results route at once.
pub fn agent_done_decision(round: u32, result: &str, spec: &Repeat) -> RepeatStep {
    match result {
        RESULT_PASSED => RepeatStep::Pass,
        "" | RESULT_NEEDS_WORK => {
            if i64::from(round) >= or_default(spec.max_rounds, DEFAULT_MAX_ROUNDS) {
                RepeatStep::MaxRounds
            } else {
                RepeatStep::Next
            }
        }
        _ => RepeatStep::Route,
    }
}
