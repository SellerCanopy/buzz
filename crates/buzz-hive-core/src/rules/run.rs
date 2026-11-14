//! The run reducer. [`step`] takes a run and one input and returns the run's next state
//! and the effects the orchestrator must carry out.
//!
//! Every side effect is data. The orchestrator writes it to the outbox in the same
//! transaction as the new run, so a crash replays it and never loses or repeats it.

use serde_json::json;
use uuid::Uuid;

use crate::domain::run::{Run, RunState};
use crate::domain::workflow::{Stage, KIND_PROMPT};
use crate::ports::store::Pubkey;
use crate::workflow::load::Package;

/// What can happen to a run. Phase 7 adds the stage results, Phase 11 the timeouts and
/// retries. `step` matches exhaustively, so a variant added without an arm fails to compile.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Start { requester: Option<Pubkey> },
}

/// What the orchestrator does after a step.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A line in the update channel saying where the card is. The orchestrator puts the
    /// card's title in front of `text`. `key` is the outbox idempotency key.
    PostStatus {
        card: Uuid,
        key: String,
        text: String,
    },
    /// A top-level message in the update channel that mentions the stage's agent and asks
    /// it to do the stage. The orchestrator fills in the text and the agent's pubkey.
    PostStageTask {
        card: Uuid,
        stage: String,
        visit: u32,
        /// The agent's name as the workflow writes it.
        agent: String,
        key: String,
    },
    /// An event for the log.
    Emit {
        kind: &'static str,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub run: Run,
    pub effects: Vec<Effect>,
}

/// The outbox key of a stage's task: `kiln:{card}:{stage}:{visit}`, the Go ActionKey shape.
pub fn task_key(card: Uuid, stage: &str, visit: u32) -> String {
    format!("kiln:{card}:{stage}:{visit}")
}

/// The outbox key of the status line that goes with a stage's task.
pub fn status_key(card: Uuid, stage: &str, visit: u32) -> String {
    format!("{}:status", task_key(card, stage, visit))
}

/// The stage a run starts at and the name of the Buzz agent that works it. An error is a
/// sentence for the person starting the run.
pub fn entry_stage(wf: &Package) -> Result<(&Stage, &str), String> {
    let stage = wf
        .workflow
        .stages
        .first()
        .ok_or_else(|| format!("workflow {} has no stages", wf.meta.name))?;
    if stage.kind != KIND_PROMPT {
        return Err(format!(
            "stage {} is a {} stage, and only prompt stages run so far",
            stage.id, stage.kind
        ));
    }
    let agent = stage
        .agent
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "stage {} names no agent; set agent in the workflow",
                stage.id
            )
        })?;
    Ok((stage, agent))
}

/// Pure: no clock, no store, no network. An input that does not apply to the run's state
/// returns the run unchanged with no effects, so a repeated input is harmless.
pub fn step(run: &Run, wf: &Package, input: Input) -> Step {
    match input {
        Input::Start { requester } => start(run, wf, requester),
    }
}

fn start(run: &Run, wf: &Package, requester: Option<Pubkey>) -> Step {
    let unchanged = || Step {
        run: run.clone(),
        effects: Vec::new(),
    };
    if run.state != RunState::Created {
        return unchanged();
    }
    // The orchestrator checks the entry stage before it makes a run, so this is the guard
    // for a caller that did not.
    let Ok((stage, agent)) = entry_stage(wf) else {
        return unchanged();
    };
    // `run.visit` holds the visits the card has made to this stage before, in earlier runs.
    let visit = run.visit + 1;
    let mut next = run.clone();
    next.state = RunState::Running;
    next.current_stage = Some(stage.id.clone());
    next.visit = visit;
    next.requester = requester.clone();
    let name = if stage.name.is_empty() {
        &stage.id
    } else {
        &stage.name
    };
    Step {
        run: next,
        effects: vec![
            Effect::Emit {
                kind: "run.started",
                payload: json!({
                    "workflow": run.workflow,
                    "stage": stage.id,
                    "visit": visit,
                    "requester": requester,
                }),
            },
            Effect::PostStatus {
                card: run.card_id,
                key: status_key(run.card_id, &stage.id, visit),
                text: format!("run started on {}, at stage {name}.", run.workflow),
            },
            Effect::PostStageTask {
                card: run.card_id,
                stage: stage.id.clone(),
                visit,
                agent: agent.to_owned(),
                key: task_key(run.card_id, &stage.id, visit),
            },
        ],
    }
}
