//! Auto mode: which Needs you items the engine answers by rule, and how. Ported from the Go
//! engine's `rules/autopilot.go`, without its cost cap or its advisory counter.
//!
//! These functions are pure. The orchestrator in `orchestrator::auto` reads what they need,
//! carries out the answer, and leaves everything else for a person.

use std::collections::HashSet;

use crate::domain::inbox::InboxKind;

/// How many times auto mode starts a failed run again before it leaves the failure to a
/// person.
pub const AUTO_RETRIES: u32 = 2;
/// How many times auto mode asks a stage that never reported to carry on.
pub const AUTO_RESUMES: u32 = 2;
/// The loop limit an auto-mode card gets from a stage whose workflow sets none.
pub const AUTO_LOOP_LIMIT: u32 = 10;

/// What auto mode has already done for the item's stage, counted from the answers it gave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tries {
    /// Failed runs auto mode started again, in a row.
    pub retries: u32,
    /// Times auto mode asked the stage's agent to carry on, in a row.
    pub resumes: u32,
}

/// Auto mode never runs a loop without a bound.
pub fn auto_loop_limit(workflow_limit: i64) -> i64 {
    if workflow_limit <= 0 {
        i64::from(AUTO_LOOP_LIMIT)
    } else {
        workflow_limit
    }
}

/// What auto mode does with an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoAnswer {
    /// Approve the approval stage.
    Approve,
    /// Start the failed run again.
    Retry,
    /// Ask the stage's agent to carry on.
    Resume,
    /// Leave it in Needs you, for `reason`.
    Person { reason: String },
}

/// A gate passes only when every artifact it lists has a saved version. An agent that calls
/// `approve_gate` is held to the same rule, whether or not the card is in auto mode.
pub fn answer_gate(listed: &[String], saved: &HashSet<String>) -> AutoAnswer {
    match listed.iter().find(|name| !saved.contains(*name)) {
        None => AutoAnswer::Approve,
        Some(missing) => AutoAnswer::Person {
            reason: format!("artifact `{missing}` has no saved version"),
        },
    }
}

/// How auto mode answers an item of `kind`. `listed` is what an approval lists and `saved` the
/// artifacts of the card that have a version. A sign-in, a choice of route, a limit and a stage
/// that skipped its outputs are always a person's.
pub fn auto_answer(
    kind: InboxKind,
    tries: &Tries,
    listed: &[String],
    saved: &HashSet<String>,
) -> AutoAnswer {
    match kind {
        InboxKind::Approval => answer_gate(listed, saved),
        InboxKind::Failure if tries.retries < AUTO_RETRIES => AutoAnswer::Retry,
        InboxKind::NoResult if tries.resumes < AUTO_RESUMES => AutoAnswer::Resume,
        _ => AutoAnswer::Person {
            reason: "needs a person".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn saved(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_gate_with_every_artifact_saved_is_approved() {
        assert_eq!(
            answer_gate(
                &names(&["plan", "logo"]),
                &saved(&["plan", "logo", "extra"])
            ),
            AutoAnswer::Approve
        );
        // A gate that lists nothing has nothing to miss.
        assert_eq!(answer_gate(&[], &saved(&[])), AutoAnswer::Approve);
    }

    #[test]
    fn a_gate_with_one_missing_artifact_goes_to_a_person_and_names_it() {
        assert_eq!(
            answer_gate(&names(&["plan", "logo"]), &saved(&["plan"])),
            AutoAnswer::Person {
                reason: "artifact `logo` has no saved version".into()
            }
        );
    }

    #[test]
    fn retries_cap_at_two() {
        let at = |retries| {
            auto_answer(
                InboxKind::Failure,
                &Tries {
                    retries,
                    resumes: 0,
                },
                &[],
                &saved(&[]),
            )
        };
        assert_eq!(at(0), AutoAnswer::Retry);
        assert_eq!(at(1), AutoAnswer::Retry);
        assert!(matches!(at(2), AutoAnswer::Person { .. }));
    }

    #[test]
    fn resumes_cap_at_two() {
        let at = |resumes| {
            auto_answer(
                InboxKind::NoResult,
                &Tries {
                    retries: 0,
                    resumes,
                },
                &[],
                &saved(&[]),
            )
        };
        assert_eq!(at(0), AutoAnswer::Resume);
        assert_eq!(at(1), AutoAnswer::Resume);
        assert!(matches!(at(2), AutoAnswer::Person { .. }));
    }

    #[test]
    fn what_needs_judgment_stays_with_a_person() {
        for kind in [
            InboxKind::Question,
            InboxKind::Route,
            InboxKind::MissingOutput,
        ] {
            assert_eq!(
                auto_answer(kind, &Tries::default(), &[], &saved(&[])),
                AutoAnswer::Person {
                    reason: "needs a person".into()
                },
                "{kind:?}"
            );
        }
    }

    #[test]
    fn an_auto_mode_loop_is_always_bounded() {
        assert_eq!(auto_loop_limit(0), 10);
        assert_eq!(auto_loop_limit(-1), 10);
        assert_eq!(auto_loop_limit(3), 3);
        assert_eq!(auto_loop_limit(50), 50);
    }
}
