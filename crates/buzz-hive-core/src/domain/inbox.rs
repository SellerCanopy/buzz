//! Needs you: the places a run waits for a person to decide.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Approve an approval gate.
pub const ANSWER_APPROVE: &str = "approve";
/// Send an approval gate back with a reason.
pub const ANSWER_REJECT: &str = "reject";
/// Answer an agent's question, or tell the agent to carry on, with text.
pub const ANSWER_REPLY: &str = "reply";
/// Move the card on as if the stage had passed.
pub const ANSWER_SKIP: &str = "skip";
/// Stop the run.
pub const ANSWER_STOP: &str = "stop";
/// Run the stage again, or start a new run of a card that failed.
pub const ANSWER_RETRY: &str = "retry";
/// Close an item that holds nothing.
pub const ANSWER_DISMISS: &str = "dismiss";
/// What a resolved item records when the run moved on without an answer, for example because
/// a person skipped the stage from the card's page.
pub const ANSWER_SUPERSEDED: &str = "superseded";

/// Why a run waits for a person. The stored text is the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    /// An approval stage waits for a person to approve or send the card back.
    Approval,
    /// The stage's agent reported `needs_input`.
    Question,
    /// A route asks a person where the card goes, or the workflow cannot go on by itself: a loop
    /// limit, a stage this build cannot run, a repeat that stalled.
    Route,
    /// The stage's time ran out and its agent never reported a result.
    NoResult,
    /// The stage reported `passed` without saving an output it requires.
    MissingOutput,
    /// The run failed for good. The run is over; a person may start another.
    Failure,
}

impl InboxKind {
    /// Every kind, in the order the lists show them.
    pub const ALL: [Self; 6] = [
        Self::Approval,
        Self::Question,
        Self::Route,
        Self::NoResult,
        Self::MissingOutput,
        Self::Failure,
    ];

    /// The value stored in `hive.inbox.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Route => "route",
            Self::NoResult => "no_result",
            Self::MissingOutput => "missing_output",
            Self::Failure => "failure",
        }
    }

    /// The inverse of [`InboxKind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The answers an item of this kind takes.
    pub fn choices(self) -> &'static [&'static str] {
        match self {
            Self::Approval => &[ANSWER_APPROVE, ANSWER_REJECT],
            Self::Question => &[ANSWER_REPLY, ANSWER_SKIP, ANSWER_STOP],
            Self::Route => &[ANSWER_SKIP, ANSWER_STOP],
            Self::NoResult | Self::MissingOutput => &[ANSWER_RETRY, ANSWER_SKIP, ANSWER_STOP],
            Self::Failure => &[ANSWER_RETRY, ANSWER_DISMISS],
        }
    }

    /// Whether the run is still alive while an item of this kind is open. A failure's run has
    /// ended, so nothing else resolves it.
    pub fn holds_run(self) -> bool {
        self != Self::Failure
    }
}

/// One Needs you item.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxItem {
    pub id: Uuid,
    pub card_id: Uuid,
    pub run_id: Uuid,
    pub stage: String,
    /// The visit of the stage the item was opened at.
    pub visit: u32,
    pub kind: InboxKind,
    /// One sentence for the list: what waits and why.
    pub title: String,
    /// Kind-specific fields. An approval lists its artifacts.
    pub detail: serde_json::Value,
    pub opened_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// The pubkey of whoever answered. For an item the engine closed itself (an auto answer, or
    /// a run that moved on) it holds the Beehive account's own pubkey, which auto mode counts on.
    pub resolved_by: Option<String>,
    /// The choice it was resolved with, `superseded` when the run moved on without one.
    pub answer: Option<String>,
}

impl InboxItem {
    /// An open item with a new id.
    pub fn open(
        card_id: Uuid,
        run_id: Uuid,
        stage: &str,
        visit: u32,
        kind: InboxKind,
        title: &str,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            card_id,
            run_id,
            stage: stage.to_owned(),
            visit,
            kind,
            title: title.to_owned(),
            detail,
            opened_at: chrono::SubsecRound::trunc_subsecs(Utc::now(), 6),
            resolved_at: None,
            resolved_by: None,
            answer: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.resolved_at.is_none()
    }

    /// The answers the item takes now: none once it is resolved.
    pub fn choices(&self) -> &'static [&'static str] {
        if self.is_open() {
            self.kind.choices()
        } else {
            &[]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_stored_name() {
        for kind in InboxKind::ALL {
            assert_eq!(InboxKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(InboxKind::parse("other"), None);
    }

    #[test]
    fn every_kind_takes_at_least_one_answer() {
        for kind in InboxKind::ALL {
            assert!(!kind.choices().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn a_resolved_item_takes_no_answer() {
        let mut item = InboxItem::open(
            Uuid::nil(),
            Uuid::nil(),
            "plan",
            1,
            InboxKind::Route,
            "asks",
            serde_json::json!({}),
        );
        assert_eq!(item.choices(), &[ANSWER_SKIP, ANSWER_STOP]);
        item.resolved_at = Some(Utc::now());
        assert!(item.choices().is_empty());
    }
}
