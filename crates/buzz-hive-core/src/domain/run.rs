//! A run: one pass of a card through its workflow.

use chrono::{DateTime, SubsecRound, Utc};
use uuid::Uuid;

/// Where a run is. The stored text is the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Made, not started. The orchestrator steps it to `Running` in the same transaction
    /// that inserts it, so a run seen outside one is never in this state.
    Created,
    Running,
    Waiting,
    Paused,
    Ended,
}

impl RunState {
    /// The value stored in `hive.runs.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Ended => "ended",
        }
    }

    /// The inverse of [`RunState::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "created" => Some(Self::Created),
            "running" => Some(Self::Running),
            "waiting" => Some(Self::Waiting),
            "paused" => Some(Self::Paused),
            "ended" => Some(Self::Ended),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: Uuid,
    pub card_id: Uuid,
    /// The workflow package name the run started with.
    pub workflow: String,
    /// The content hash of that package's version. A run keeps it, so a package edited
    /// after the start does not change a run in flight.
    pub workflow_hash: String,
    pub state: RunState,
    /// The stage the run is at, `None` before it starts.
    pub current_stage: Option<String>,
    /// Which visit to `current_stage` this is, from 1, counting the card's visits in earlier
    /// runs. Before the run starts it holds the visits made so far to the stage it will enter.
    pub visit: u32,
    /// Pubkey of whoever started the run.
    pub requester: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl Run {
    /// A run that has not started.
    pub fn new(card_id: Uuid, workflow: &str, workflow_hash: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            card_id,
            workflow: workflow.to_owned(),
            workflow_hash: workflow_hash.to_owned(),
            state: RunState::Created,
            current_stage: None,
            visit: 0,
            requester: None,
            // Microseconds, the precision the store keeps, so a stored run equals the one made.
            started_at: Utc::now().trunc_subsecs(6),
            ended_at: None,
        }
    }

    /// Whether the run still holds its card.
    pub fn is_active(&self) -> bool {
        self.state != RunState::Ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_its_stored_name() {
        for state in [
            RunState::Created,
            RunState::Running,
            RunState::Waiting,
            RunState::Paused,
            RunState::Ended,
        ] {
            assert_eq!(RunState::parse(state.as_str()), Some(state));
        }
        assert_eq!(RunState::parse("stopped"), None);
    }

    #[test]
    fn a_new_run_is_active_and_not_started() {
        let run = Run::new(Uuid::nil(), "demo", "abc");
        assert_eq!(run.state, RunState::Created);
        assert!(run.is_active());
        assert_eq!(run.current_stage, None);
    }
}
