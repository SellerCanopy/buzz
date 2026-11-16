//! A run: one pass of a card through its workflow.

use std::collections::BTreeMap;

use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
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

/// One entry of a card into a stage during a run. It stays in [`Run::steps`] after the run
/// leaves the stage, so the Steps tab, the loop limit and the visit count read from it.
///
/// A repeat stage keeps one step for all its rounds: `round` counts them and `visit` is the
/// thread of the latest one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStep {
    pub stage: String,
    /// The stage's thread counter at the latest round: the visit its task was posted under.
    pub visit: u32,
    /// The round the stage is in, from 1. Only a repeat stage goes past 1.
    #[serde(default)]
    pub round: u32,
    /// Plan items checked after the latest round, for a stage that repeats until its plan
    /// is complete.
    #[serde(default)]
    pub checked: u32,
    /// Rounds in a row that left `checked` as it was.
    #[serde(default)]
    pub unchanged: u32,
    /// Times the card was resumed while the stage was running.
    #[serde(default)]
    pub resumes: u32,
    /// The last result the stage's agent reported, empty while none has.
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub summary: String,
    /// Where the stage sent the card: a stage id, `done` or `stop`. `None` while the card is
    /// still in the stage, and when the route was refused or went to `ask`.
    #[serde(default)]
    pub routed_to: Option<String>,
    /// Whether that route went to this stage or an earlier one.
    #[serde(default)]
    pub backward: bool,
    /// The workflow holds the card before this stage, so its task is not posted until a
    /// person resumes the card.
    #[serde(default)]
    pub held: bool,
    /// The stage never ran: its conditions were not met, or a person skipped it.
    #[serde(default)]
    pub skipped: bool,
    /// The stage that called this one with a `then`: where the card goes once this stage
    /// passes. Empty when it follows the stage's own route.
    #[serde(default)]
    pub then: String,
    /// Attempts at the stage that failed, so the next one is `attempts + 1`. A retry keeps the
    /// stage's thread and visit.
    #[serde(default)]
    pub attempts: u32,
    /// Why the latest attempt failed, for the note that tells the agent to try again. Empty
    /// while no attempt has failed.
    #[serde(default)]
    pub failure: String,
    /// The comments a person sent the card back with from an approval stage, which this stage
    /// gets with its task. Empty when the card did not come back to it.
    #[serde(default)]
    pub sendback: String,
    /// Answers a person has given the stage's agent while the run waited at the stage.
    #[serde(default)]
    pub answers: u32,
    /// When the stage was entered, as the store saw it. A wait for CI gives checks that have not
    /// appeared yet a grace period from here. `None` for a step made before it was kept.
    #[serde(default)]
    pub entered_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// Every stage the run has entered, oldest first. The last one is the current stage.
    pub steps: Vec<RunStep>,
    /// The run's own parameter values by name, which override the card's and the workflow's.
    pub parameters: BTreeMap<String, String>,
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
            steps: Vec::new(),
            parameters: BTreeMap::new(),
            // Microseconds, the precision the store keeps, so a stored run equals the one made.
            started_at: Utc::now().trunc_subsecs(6),
            ended_at: None,
        }
    }

    /// The step the run is in, if it has entered a stage.
    pub fn current_step(&self) -> Option<&RunStep> {
        self.steps.last()
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
