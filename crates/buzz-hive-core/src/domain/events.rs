//! The event log: one row per change, with the person or agent that made it.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::access::ActorKind;

/// Who caused an event. Counting events by `kind` is how cards created by agents are measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub pubkey: String,
    pub kind: ActorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Assigned by the store on commit; 0 before.
    pub seq: i64,
    pub kind: String,
    pub card_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub actor: Actor,
    pub payload: serde_json::Value,
    pub at: DateTime<Utc>,
}

impl Event {
    /// A new event with no seq yet, stamped with the current time.
    pub fn new(
        kind: &str,
        card_id: Option<Uuid>,
        run_id: Option<Uuid>,
        actor: Actor,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            seq: 0,
            kind: kind.to_owned(),
            card_id,
            run_id,
            actor,
            payload,
            at: Utc::now(),
        }
    }
}

pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Every kind the engine may append, with the phase that introduces it.
///
/// `check_new` refuses a kind not listed here. `tests/event_coverage.rs` needs every
/// kind introduced so far to be appended by some scenario.
pub const EVENT_KINDS: &[(&str, u8)] = &[
    ("card.created", 4),
    ("card.moved", 4),
    ("card.archived", 4),
    ("card.restored", 4),
    ("card.removed", 4),
    ("card.state", 4),
    // `AddNote` appends it in Phase 4, ahead of the activity feed in Phase 12.
    ("activity.added", 4),
    ("setting.changed", 6),
    ("run.started", 6),
    // A stage task reached the relay and opened its thread.
    ("stage.posted", 6),
    // The stage's agent reported a result through `stage_done`.
    ("result.reported", 7),
    // The run ended. Its payload names the outcome.
    ("run.ended", 7),
    // The run left a stage for another, or for the end. The payload has the result and
    // whether the route went backward.
    ("stage.routed", 8),
    // The run waits for a person: a route that asks, a loop limit, a stage this build cannot
    // run, or a repeat that stalled. The payload has the reason.
    ("run.waiting", 8),
    // The run is held before a stage, or a person paused it.
    ("run.paused", 8),
    ("run.resumed", 8),
    // A stage never ran: its conditions were not met, or a person skipped it.
    ("card.skipped", 8),
    ("condition.unmet", 8),
    // A round of a repeat stage began, and ended with the plan's progress.
    ("round.started", 8),
    ("round.ended", 8),
    // A stage saved a version of an artifact. The payload names the artifact, its version,
    // its kind and the stage.
    ("artifact.saved", 9),
    // A person set whether later stages get an artifact.
    ("artifact.handoff", 9),
    // A stage's attempt failed and will run again. The payload has the stage, the attempt
    // that failed, how many seconds until the next one, and why it failed.
    ("retry.scheduled", 11),
    // A run began waiting for a person, and an item was opened in Needs you. The payload has the
    // item's id, kind, stage and title.
    ("inbox.opened", 12),
    // A person answered an item, or the run moved on and the engine closed it. The payload has
    // the item's id, the answer, and who answered.
    ("inbox.resolved", 12),
    // A person approved an approval stage. The payload has the stage and the artifact versions.
    ("approval.recorded", 12),
    // A person sent an approval stage back. The payload has the stage and the comments.
    ("card.sent_back", 12),
    // An admin set a person's or an agent's access level. The payload has the pubkey, its
    // kind, the level it had and the level it has now.
    ("access.changed", 14),
    // A pass over an intake rule read the host. The payload has the rule, the repo, how many
    // issues matched, how many cards were added and how many issues left the search.
    ("intake.refreshed", 16),
    // A pass over an intake rule failed. The payload has the rule, the repo, why, and how many
    // passes in a row have failed.
    ("intake.failed", 16),
    // A card's issue is no longer in its rule's search. The payload has the rule, the repo, the
    // issue number and why (closed or unmatched).
    ("card.left_intake", 16),
    // A comment appeared on a card's issue. The payload has the repo, the issue number, the
    // comment's id and author, and whether it was handed to the stage's agent.
    ("comment.received", 16),
    // A comment on a card's issue was queued into the stage's thread. The payload names the
    // stage, the visit and the comment.
    ("comment.delivered", 16),
    // The sync read a card's pull request and it differs from what Beehive kept: its state,
    // whether it is a draft or approved, or how its checks stand. The payload has the repo, the
    // number and each of those.
    ("pull_request.changed", 18),
    // The assistant's agent was asked for a job. The payload has the job id, its kind, who it is
    // for, the thread the agent works in and the agent.
    ("job.started", 20),
    // A job ended, once, by the agent's result or by running out of time. A cancelled job has
    // none. The payload has the job id, its kind, its state (done or failed), the agent's
    // message and the proposal it stored, or why it failed.
    ("job.result", 20),
    // A repo was added to a card or taken off it. The payload has the card's repos now, first
    // repo first, and the repo `added` or `removed`.
    ("card.repos", 21),
];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    #[error("unknown event kind {0}")]
    UnknownKind(String),
    #[error("event payload is over {MAX_PAYLOAD_BYTES} bytes")]
    TooLarge,
}

/// Refuses a kind the registry does not list and a payload over [`MAX_PAYLOAD_BYTES`].
pub fn check_new(kind: &str, payload: &serde_json::Value) -> Result<(), EventError> {
    if !EVENT_KINDS.iter().any(|(k, _)| *k == kind) {
        return Err(EventError::UnknownKind(kind.to_owned()));
    }
    if serde_json::to_vec(payload)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
        > MAX_PAYLOAD_BYTES
    {
        return Err(EventError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_listed_kind_with_a_small_payload_passes() {
        assert_eq!(check_new("card.created", &json!({"title": "x"})), Ok(()));
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        assert_eq!(
            check_new("card.exploded", &json!({})),
            Err(EventError::UnknownKind("card.exploded".into()))
        );
    }

    #[test]
    fn a_payload_over_64_kib_is_refused() {
        let big = json!({ "text": "x".repeat(MAX_PAYLOAD_BYTES) });
        assert_eq!(check_new("card.created", &big), Err(EventError::TooLarge));
        let just_under = json!({ "text": "x".repeat(MAX_PAYLOAD_BYTES - 64) });
        assert_eq!(check_new("card.created", &just_under), Ok(()));
    }

    #[test]
    fn kinds_are_unique() {
        let mut names: Vec<_> = EVENT_KINDS.iter().map(|(k, _)| *k).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), EVENT_KINDS.len());
    }
}
