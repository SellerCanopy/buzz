//! The run reducer. [`step`] takes a run and one input and returns the run's next state
//! and the effects the orchestrator must carry out.
//!
//! Every side effect is data. The orchestrator writes it to the outbox in the same
//! transaction as the new run, so a crash replays it and never loses or repeats it.
//!
//! A run is one pass of a card through its workflow, so one run holds several stages. Each
//! time the run enters a stage it bumps that stage's visit, and each visit is a new
//! top-level mention in the update channel, so each stage is a new agent session.

use std::collections::BTreeMap;

use serde_json::json;
use uuid::Uuid;

use super::conditions::{describe, unmet};
use super::decide::{loop_count, over_loop_limit, resolve_route, routes_backward, Route};
use super::repeat::{
    agent_done_decision, is_agent_done, or_default, repeat_decision, RepeatState, RepeatStep,
    DEFAULT_MAX_ROUNDS, DEFAULT_STALL_AFTER,
};
use crate::domain::run::{Run, RunState, RunStep};
use crate::domain::workflow::{Stage, KIND_PROMPT, RESULT_PASSED};
use crate::ports::store::Pubkey;
use crate::workflow::load::Package;
use crate::workflow::routes::ROUTE_ASK;

/// What can happen to a run. Phase 11 adds the timeouts and retries. `step` matches
/// exhaustively, so a variant added without an arm fails to compile.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A person, or an agent, starts the run. `stage` is where; `None` starts at the first
    /// stage. `held` makes the entry stage honor the workflow's pauses, as a card handed off
    /// from another workflow does. A person who starts a stage by hand is not held.
    Start {
        requester: Option<Pubkey>,
        stage: Option<String>,
        held: bool,
    },
    /// The agent of `stage` reported `result`. `summary` is what it said about it, possibly empty.
    StageDone {
        stage: String,
        result: String,
        summary: String,
    },
    /// A person moves the card on as if the current stage had passed.
    Skip,
    /// A person holds a running run. The stage's agent may not report until it resumes.
    Pause,
    /// A person lets a held stage start, or a paused one carry on.
    Resume,
    /// The stage's agent moves the card to `stage`, in place of the route its result gives.
    Move { stage: String },
    /// The stage's agent reported `passed`, or moved the card, without saving every output the
    /// stage requires. The run waits for a person instead of going on.
    MissingOutput {
        stage: String,
        missing: Vec<String>,
        summary: String,
    },
}

/// What the reducer needs to know that is not in the run or the workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// The card's labels. They come from the card's issue, so there are none until intake exists.
    pub labels: Vec<String>,
    /// The stages the card waits before until a person starts it.
    pub paused: Vec<String>,
    /// The highest visit the card has made to each stage across all its runs, this one included.
    pub visits: BTreeMap<String, u32>,
    /// The checked and the total items of the plan a `plan_complete` repeat counts, as they are
    /// after the round that just ended: the latest version of the stage's plan artifact, or
    /// `(0, 0)` when it has none.
    pub plan: (u32, u32),
}

/// What a repeat stage tells the agent about its next round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub number: u32,
    /// Plan items checked, and the plan's name; empty for a stage that repeats until its
    /// agent reports done.
    pub checked: u32,
    pub total: u32,
    pub plan: String,
}

/// What a reply in a stage's thread says beyond the stage's task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The stage's task again, for the next round of a repeat stage.
    Round(Round),
    /// The card was resumed while the stage was running.
    Resumed,
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
    /// it to do the stage. The orchestrator fills in the text and the agent's pubkey. A
    /// `round` past the first is a repeat stage's next round in a fresh session.
    PostStageTask {
        card: Uuid,
        stage: String,
        visit: u32,
        /// The agent's name as the workflow writes it.
        agent: String,
        key: String,
        round: Option<Round>,
    },
    /// A reply in the thread the stage's task opened, which mentions the stage's agent.
    PostReply {
        card: Uuid,
        stage: String,
        /// The visit whose thread the reply goes in.
        visit: u32,
        agent: String,
        key: String,
        note: Note,
    },
    /// An event for the log.
    Emit {
        kind: &'static str,
        payload: serde_json::Value,
    },
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The workflow ran to its end, and the card is done.
    Done,
    /// A route stopped the card.
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub run: Run,
    pub effects: Vec<Effect>,
    /// Set when the step ended the run.
    pub ended: Option<Ended>,
    /// Why the run waits or is held, when the step left it waiting or paused.
    pub reason: Option<String>,
}

impl Step {
    fn unchanged(run: &Run) -> Self {
        Self {
            run: run.clone(),
            effects: Vec::new(),
            ended: None,
            reason: None,
        }
    }
}

/// The outbox key of a stage's task: `kiln:{card}:{stage}:{visit}`, the Go ActionKey shape.
pub fn task_key(card: Uuid, stage: &str, visit: u32) -> String {
    format!("kiln:{card}:{stage}:{visit}")
}

/// The outbox key of the status line that goes with a stage's task.
pub fn status_key(card: Uuid, stage: &str, visit: u32) -> String {
    format!("{}:status", task_key(card, stage, visit))
}

/// The outbox key of a reply that opens `round` in the thread of `visit`.
pub fn round_key(card: Uuid, stage: &str, visit: u32, round: u32) -> String {
    format!("{}:round:{round}", task_key(card, stage, visit))
}

/// The outbox key of the `resume`th reply that tells a resumed stage's agent to carry on.
pub fn resume_key(card: Uuid, stage: &str, visit: u32, resume: u32) -> String {
    format!("{}:resume:{resume}", task_key(card, stage, visit))
}

/// The stage a run starts at and the name of the Buzz agent that works it. `stage` is where
/// the person asked to start, or `None` for the first stage. An error is a sentence for the
/// person starting the run.
pub fn entry_stage<'a>(
    wf: &'a Package,
    stage: Option<&str>,
) -> Result<(&'a Stage, &'a str), String> {
    let stage = match stage {
        Some(id) => wf
            .workflow
            .stages
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("workflow {} has no stage {id}", wf.meta.name))?,
        None => wf
            .workflow
            .stages
            .first()
            .ok_or_else(|| format!("workflow {} has no stages", wf.meta.name))?,
    };
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
pub fn step(run: &Run, wf: &Package, facts: &Facts, input: Input) -> Step {
    match input {
        Input::Start {
            requester,
            stage,
            held,
        } => start(run, wf, facts, requester, stage.as_deref(), held),
        Input::StageDone {
            stage,
            result,
            summary,
        } => stage_done(run, wf, facts, &stage, &result, &summary),
        Input::Skip => skip(run, wf, facts),
        Input::Pause => pause(run),
        Input::Resume => resume(run, wf, facts),
        Input::Move { stage } => move_to(run, wf, facts, &stage),
        Input::MissingOutput {
            stage,
            missing,
            summary,
        } => missing_output(run, wf, facts, &stage, &missing, &summary),
    }
}

/// The name a stage goes by in a sentence.
fn title(stage: &Stage) -> &str {
    if stage.name.is_empty() {
        &stage.id
    } else {
        &stage.name
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The most stages one input may enter. A workflow whose conditions send the card round in a
/// circle would otherwise never return.
const MAX_HOPS: u32 = 64;

/// One input being applied: the run as it changes, and what it asks for so far.
struct Walk<'a> {
    run: Run,
    wf: &'a Package,
    facts: &'a Facts,
    visits: BTreeMap<String, u32>,
    effects: Vec<Effect>,
    ended: Option<Ended>,
    reason: Option<String>,
    hops: u32,
}

impl<'a> Walk<'a> {
    fn new(run: &Run, wf: &'a Package, facts: &'a Facts) -> Self {
        let mut run = run.clone();
        // A run made before steps were kept has none. Its current stage is the one step.
        if run.steps.is_empty() {
            if let Some(stage) = run.current_stage.clone() {
                run.steps.push(RunStep {
                    stage,
                    visit: run.visit,
                    round: 1,
                    ..RunStep::default()
                });
            }
        }
        Self {
            run,
            wf,
            facts,
            visits: facts.visits.clone(),
            effects: Vec::new(),
            ended: None,
            reason: None,
            hops: 0,
        }
    }

    fn stages(&self) -> &'a [Stage] {
        &self.wf.workflow.stages
    }

    fn emit(&mut self, kind: &'static str, payload: serde_json::Value) {
        self.effects.push(Effect::Emit { kind, payload });
    }

    fn finish(self) -> Step {
        Step {
            run: self.run,
            effects: self.effects,
            ended: self.ended,
            reason: self.reason,
        }
    }

    fn current(&mut self) -> &mut RunStep {
        self.run
            .steps
            .last_mut()
            .expect("a run that has entered a stage has a step")
    }

    /// The run waits for a person, who answers by skipping the stage or moving the card.
    fn wait(&mut self, reason: String) {
        self.wait_for(reason, json!({}));
    }

    /// Like [`Self::wait`], with more fields in the `run.waiting` payload.
    fn wait_for(&mut self, reason: String, extra: serde_json::Value) {
        self.run.state = RunState::Waiting;
        let mut payload =
            json!({ "stage": self.run.current_stage, "visit": self.run.visit, "reason": reason });
        if let (Some(fields), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            fields.extend(extra.clone());
        }
        self.emit("run.waiting", payload);
        self.reason = Some(reason);
    }

    /// The next visit to `stage`: one past the highest the card has made.
    fn bump(&mut self, stage: &str) -> u32 {
        let visit = self.visits.get(stage).copied().unwrap_or(0) + 1;
        self.visits.insert(stage.to_owned(), visit);
        visit
    }

    /// Ends the run where the workflow sends it.
    fn end(&mut self, ended: Ended) {
        let (outcome, target) = match ended {
            Ended::Done => (RESULT_PASSED, "done"),
            Ended::Stopped => ("stopped", "stop"),
        };
        self.run.state = RunState::Ended;
        self.ended = Some(ended);
        self.current().routed_to = Some(target.to_owned());
        self.emit(
            "run.ended",
            json!({ "outcome": outcome, "stage": self.run.current_stage, "visit": self.run.visit }),
        );
    }

    /// Follows `route` out of the stage at `from`, the run's current stage. `said` says what
    /// sent the card, as in `reported failed`, for the sentence a person reads when the run
    /// waits. `limited` applies the stage's loop limit to a route that goes backward.
    fn route(&mut self, from: usize, route: Route, said: &str, limited: bool) {
        let stages = self.stages();
        let stage = &stages[from];
        match route {
            Route::Done => self.end(Ended::Done),
            Route::Stop => self.end(Ended::Stopped),
            Route::Ask => self.wait(format!(
                "{} {said}: choose where the card goes",
                title(stage)
            )),
            Route::Stage(ref to) | Route::Call { ref to, .. } => {
                let Some(to_idx) = stages.iter().position(|s| s.id == *to) else {
                    self.wait(format!(
                        "{} {said}, which routes to {to}, a stage the workflow does not have",
                        title(stage)
                    ));
                    return;
                };
                let backward = routes_backward(stages, from, to);
                if limited && backward && stage.loop_limit > 0 {
                    // The entries before this one, oldest first.
                    let steps = &self.run.steps;
                    let earlier: Vec<bool> = steps[..steps.len().saturating_sub(1)]
                        .iter()
                        .filter(|s| s.stage == stage.id)
                        .map(|s| s.backward)
                        .collect();
                    let loops = loop_count(&earlier);
                    if over_loop_limit(stage.loop_limit, loops) {
                        // The route did not happen, so the next backward route counts from one.
                        self.wait(format!(
                            "{} {said}, which routes back to {to} for the {} time; its loop limit is {}",
                            title(stage),
                            ordinal(loops + 1),
                            stage.loop_limit
                        ));
                        return;
                    }
                }
                let then = match &route {
                    Route::Call { then, .. } => then.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                let result = self.run.steps.last().map(|s| s.result.clone());
                let step = self.current();
                step.routed_to = Some(to.clone());
                step.backward = backward;
                let payload = json!({
                    "from": stage.id,
                    "to": to,
                    "result": result.unwrap_or_default(),
                    "backward": backward,
                });
                self.emit("stage.routed", payload);
                self.enter(to_idx, then, true);
            }
        }
    }

    /// Enters the stage at `idx`: a new step and a new visit. A stage the workflow holds waits
    /// there for a person, unless `honor_pause` is off.
    fn enter(&mut self, idx: usize, then: String, honor_pause: bool) {
        self.hops += 1;
        let stage = &self.stages()[idx];
        if self.hops > MAX_HOPS {
            self.wait(format!(
                "the workflow's routes and conditions send the card in a circle at {}",
                title(stage)
            ));
            return;
        }
        let visit = self.bump(&stage.id);
        self.run.current_stage = Some(stage.id.clone());
        self.run.visit = visit;
        let held = honor_pause && self.facts.paused.contains(&stage.id);
        self.run.steps.push(RunStep {
            stage: stage.id.clone(),
            visit,
            round: 1,
            then,
            held,
            ..RunStep::default()
        });
        if held {
            self.run.state = RunState::Paused;
            let reason = format!("{} is held until a person resumes the card", title(stage));
            self.emit(
                "run.paused",
                json!({ "stage": stage.id, "visit": visit, "reason": reason }),
            );
            self.reason = Some(reason);
            return;
        }
        self.launch(idx);
    }

    /// Starts the stage the run is in: checks its conditions, then asks its agent to do it.
    fn launch(&mut self, idx: usize) {
        let stage = &self.stages()[idx];
        if let Some(conditions) = unmet(stage, &self.facts.labels) {
            // The stage never opens a thread. The card follows its else route.
            let condition = describe(conditions);
            self.current().skipped = true;
            self.emit(
                "condition.unmet",
                json!({ "stage": stage.id, "condition": condition, "else": conditions.else_to }),
            );
            self.emit(
                "card.skipped",
                json!({ "stage": stage.id, "reason": format!("condition unmet: {condition}") }),
            );
            let target = if conditions.else_to.is_empty() {
                ROUTE_ASK
            } else {
                conditions.else_to.as_str()
            };
            let said = format!("was skipped because the card lacks {condition}");
            self.route(idx, Route::from_target(target), &said, false);
            return;
        }
        if stage.kind != KIND_PROMPT {
            self.wait(format!(
                "{} is a {} stage, and only prompt stages run so far; skip it to carry on",
                title(stage),
                stage.kind
            ));
            return;
        }
        let Some(agent) = stage.agent.as_deref().filter(|a| !a.trim().is_empty()) else {
            self.wait(format!(
                "{} names no agent; set agent in the workflow",
                title(stage)
            ));
            return;
        };
        self.run.state = RunState::Running;
        self.reason = None;
        let card = self.run.card_id;
        let visit = self.run.visit;
        self.effects.push(Effect::PostStageTask {
            card,
            stage: stage.id.clone(),
            visit,
            agent: agent.to_owned(),
            key: task_key(card, &stage.id, visit),
            round: None,
        });
    }

    /// What a repeat stage does after a round that reported `result`.
    fn repeat_round(&mut self, idx: usize, result: &str) {
        let stage = &self.stages()[idx];
        let spec = stage.repeat.as_ref().expect("the caller checked repeat");
        let agent_done = is_agent_done(spec);
        let (checked, total) = if agent_done { (0, 0) } else { self.facts.plan };
        let now = self.current().clone();
        self.emit(
            "round.ended",
            json!({ "stage": stage.id, "round": now.round, "checked": checked, "total": total }),
        );
        let decision = if agent_done {
            agent_done_decision(now.round, result, spec)
        } else {
            let (decision, state) = repeat_decision(
                RepeatState {
                    last_checked: now.checked,
                    unchanged: now.unchanged,
                },
                now.round,
                result,
                checked,
                total,
                spec,
            );
            let step = self.current();
            step.checked = state.last_checked;
            step.unchanged = state.unchanged;
            decision
        };
        let name = title(stage);
        match decision {
            RepeatStep::Next => {
                let number = now.round + 1;
                self.current().round = number;
                self.emit(
                    "round.started",
                    json!({ "stage": stage.id, "round": number }),
                );
                let agent = stage.agent.clone().unwrap_or_default();
                let round = Round {
                    number,
                    checked,
                    total,
                    plan: if agent_done {
                        String::new()
                    } else {
                        spec.plan.clone()
                    },
                };
                let card = self.run.card_id;
                if spec.fresh_session {
                    // A new top-level mention, so a new session.
                    let visit = self.bump(&stage.id);
                    self.run.visit = visit;
                    self.current().visit = visit;
                    self.effects.push(Effect::PostStageTask {
                        card,
                        stage: stage.id.clone(),
                        visit,
                        agent,
                        key: task_key(card, &stage.id, visit),
                        round: Some(round),
                    });
                } else {
                    self.effects.push(Effect::PostReply {
                        card,
                        stage: stage.id.clone(),
                        visit: now.visit,
                        agent,
                        key: round_key(card, &stage.id, now.visit, number),
                        note: Note::Round(round),
                    });
                }
            }
            RepeatStep::Pass => {
                let step = self.current();
                step.result = RESULT_PASSED.to_owned();
                self.follow_result(idx, RESULT_PASSED);
            }
            RepeatStep::Route => self.follow_result(idx, result),
            RepeatStep::NoCheckboxes => self.wait(format!(
                "{name}: the plan {} has no checkboxes (0 items) after round {}, so progress cannot be measured",
                spec.plan, now.round
            )),
            RepeatStep::Stalled => self.wait(format!(
                "{name} stalled at {checked} of {total} plan items: no progress for {} rounds",
                or_default(spec.stall_after, DEFAULT_STALL_AFTER)
            )),
            RepeatStep::MaxRounds => {
                let limit = or_default(spec.max_rounds, DEFAULT_MAX_ROUNDS);
                if agent_done {
                    self.wait(format!(
                        "{name} reached its limit of {limit} rounds without reporting done"
                    ));
                } else {
                    self.wait(format!(
                        "{name} reached its limit of {limit} rounds at {checked} of {total} plan items"
                    ));
                }
            }
        }
    }

    /// Routes out of the stage at `idx` on `result`, the way its `on` entries and the file
    /// order say, and a `then` from the call that entered the stage when it passed.
    fn follow_result(&mut self, idx: usize, result: &str) {
        let stages = self.stages();
        let then = self
            .run
            .steps
            .last()
            .map(|s| s.then.clone())
            .unwrap_or_default();
        let next = stages.get(idx + 1).map(|s| s.id.as_str());
        let route = resolve_route(&stages[idx], next, result, &then);
        let said = if result.is_empty() {
            "was moved on".to_owned()
        } else {
            format!("reported {result}")
        };
        self.route(idx, route, &said, true);
    }
}

fn start(
    run: &Run,
    wf: &Package,
    facts: &Facts,
    requester: Option<Pubkey>,
    stage: Option<&str>,
    held: bool,
) -> Step {
    if run.state != RunState::Created {
        return Step::unchanged(run);
    }
    // The orchestrator checks the entry stage before it makes a run, so this is the guard
    // for a caller that did not.
    let Ok((entry, _)) = entry_stage(wf, stage) else {
        return Step::unchanged(run);
    };
    let Some(idx) = wf.workflow.stages.iter().position(|s| s.id == entry.id) else {
        return Step::unchanged(run);
    };
    let mut walk = Walk::new(run, wf, facts);
    walk.run.requester = requester.clone();
    let visit_after = walk.visits.get(&entry.id).copied().unwrap_or(0) + 1;
    walk.emit(
        "run.started",
        json!({
            "workflow": run.workflow,
            "stage": entry.id,
            "visit": visit_after,
            "requester": requester,
        }),
    );
    walk.effects.push(Effect::PostStatus {
        card: run.card_id,
        key: status_key(run.card_id, &entry.id, visit_after),
        text: format!(
            "run started on {}, at stage {}.",
            run.workflow,
            title(entry)
        ),
    });
    walk.enter(idx, String::new(), held);
    walk.finish()
}

/// Records the result and routes the card on it.
fn stage_done(
    run: &Run,
    wf: &Package,
    facts: &Facts,
    stage: &str,
    result: &str,
    summary: &str,
) -> Step {
    let current = run.state == RunState::Running && run.current_stage.as_deref() == Some(stage);
    let Some((_, idx)) = wf.stage(stage).filter(|_| current) else {
        return Step::unchanged(run);
    };
    let mut walk = Walk::new(run, wf, facts);
    let round = walk.current().round;
    walk.emit(
        "result.reported",
        json!({
            "stage": stage,
            "visit": run.visit,
            "round": round,
            "result": result,
            "summary": summary,
        }),
    );
    let step = walk.current();
    step.result = result.to_owned();
    step.summary = summary.to_owned();
    if wf.workflow.stages[idx].repeat.is_some() {
        walk.repeat_round(idx, result);
    } else {
        walk.follow_result(idx, result);
    }
    walk.finish()
}

/// The stage's agent reported `passed` but left out a required output. The report is logged,
/// and the run waits for a person, who skips the stage or moves the card.
fn missing_output(
    run: &Run,
    wf: &Package,
    facts: &Facts,
    stage: &str,
    missing: &[String],
    summary: &str,
) -> Step {
    let current = run.state == RunState::Running && run.current_stage.as_deref() == Some(stage);
    let Some((def, _)) = wf.stage(stage).filter(|_| current) else {
        return Step::unchanged(run);
    };
    let mut walk = Walk::new(run, wf, facts);
    let round = walk.current().round;
    walk.emit(
        "result.reported",
        json!({
            "stage": stage,
            "visit": run.visit,
            "round": round,
            "result": RESULT_PASSED,
            "summary": summary,
        }),
    );
    walk.current().summary = summary.to_owned();
    let reason = format!(
        "{} reported passed without saving {}",
        title(def),
        missing.join(", ")
    );
    walk.wait_for(
        reason,
        json!({ "condition": CONDITION_MISSING_OUTPUT, "missing": missing }),
    );
    walk.finish()
}

/// The condition a `run.waiting` event names when a stage passed without a required output.
/// Phase 12 opens a Needs you item of this kind.
pub const CONDITION_MISSING_OUTPUT: &str = "missing_output";

/// Moves a waiting or held card on as if its stage had passed.
fn skip(run: &Run, wf: &Package, facts: &Facts) -> Step {
    let Some((_, idx)) = skippable(run, wf) else {
        return Step::unchanged(run);
    };
    let mut walk = Walk::new(run, wf, facts);
    let stage = &wf.workflow.stages[idx];
    walk.current().skipped = true;
    walk.current().held = false;
    walk.emit(
        "card.skipped",
        json!({ "stage": stage.id, "reason": "a person skipped the stage" }),
    );
    walk.follow_result(idx, RESULT_PASSED);
    walk.finish()
}

/// The stage a skip leaves, when the run is waiting or held.
fn skippable<'a>(run: &Run, wf: &'a Package) -> Option<(&'a Stage, usize)> {
    if !matches!(run.state, RunState::Waiting | RunState::Paused) {
        return None;
    }
    wf.stage(run.current_stage.as_deref()?)
}

fn pause(run: &Run) -> Step {
    if run.state != RunState::Running {
        return Step::unchanged(run);
    }
    let mut next = run.clone();
    next.state = RunState::Paused;
    let reason = "a person paused the card".to_owned();
    Step {
        run: next,
        effects: vec![Effect::Emit {
            kind: "run.paused",
            payload: json!({ "stage": run.current_stage, "visit": run.visit, "reason": reason }),
        }],
        ended: None,
        reason: Some(reason),
    }
}

/// Lets a held stage start, or tells the agent of a paused one to carry on.
fn resume(run: &Run, wf: &Package, facts: &Facts) -> Step {
    let Some((stage, idx)) = run
        .current_stage
        .as_deref()
        .and_then(|id| wf.stage(id))
        .filter(|_| run.state == RunState::Paused)
    else {
        return Step::unchanged(run);
    };
    let mut walk = Walk::new(run, wf, facts);
    walk.emit(
        "run.resumed",
        json!({ "stage": stage.id, "visit": run.visit }),
    );
    if walk.current().held {
        walk.current().held = false;
        walk.launch(idx);
        return walk.finish();
    }
    // The task was posted before the pause, so tell its agent the stage is open again.
    walk.run.state = RunState::Running;
    let step = walk.current();
    step.resumes += 1;
    let (resumes, visit) = (step.resumes, step.visit);
    let card = run.card_id;
    walk.effects.push(Effect::PostReply {
        card,
        stage: stage.id.clone(),
        visit,
        agent: stage.agent.clone().unwrap_or_default(),
        key: resume_key(card, &stage.id, visit, resumes),
        note: Note::Resumed,
    });
    walk.finish()
}

/// The stage's agent moves the card to `target`, which replaces the route its result gives. The
/// move counts toward the stage's loop limit like any other backward route.
fn move_to(run: &Run, wf: &Package, facts: &Facts, target: &str) -> Step {
    let Some((_, idx)) = run
        .current_stage
        .as_deref()
        .and_then(|id| wf.stage(id))
        .filter(|_| run.state == RunState::Running)
    else {
        return Step::unchanged(run);
    };
    if wf.stage(target).is_none() {
        return Step::unchanged(run);
    }
    let mut walk = Walk::new(run, wf, facts);
    walk.route(idx, Route::Stage(target.to_owned()), "moved the card", true);
    walk.finish()
}
