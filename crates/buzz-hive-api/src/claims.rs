//! `POST /hive/v1/claim`: a signed statement of the person an agent acts for.
//!
//! An agent acts at the lower of its own level and that person's, so the person must come
//! from something the agent cannot write itself: a chat message the relay holds, or a run
//! the engine holds. The agent only names which one.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use buzz_hive_core::access::ActorKind;
use buzz_hive_core::ports::store::{RelayEvent, MESSAGE_KINDS};

use crate::app::App;
use crate::error::HiveError;
use crate::unary::caller;

/// How old a chat message may be and still name a requester. A turn can run long, and its
/// tool calls claim again as the 5 minute claim expires, so this has to outlast a turn.
pub const EVENT_MAX_AGE_SECS: i64 = 2 * 60 * 60;

#[derive(serde::Deserialize)]
pub struct ClaimRequest {
    /// The chat message that started the agent's turn, 64 hex characters.
    #[serde(default)]
    pub event_id: String,
    /// A run whose current stage this agent holds.
    #[serde(default)]
    pub run_id: String,
}

#[derive(serde::Serialize)]
pub struct ClaimReply {
    pub claim: String,
    /// Seconds since the epoch.
    pub expires_at: i64,
    /// The person the claim names, for the agent's own log.
    pub requester: String,
}

pub async fn claim(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ClaimReply>, HiveError> {
    let agent = caller(&app, &headers).await?;
    let req: ClaimRequest = serde_json::from_slice(if body.is_empty() { b"{}" } else { &body })
        .map_err(|e| HiveError::InvalidArgument(format!("invalid request body: {e}")))?;
    if agent.kind != ActorKind::Agent {
        return Err(HiveError::PermissionDenied(
            "only an agent asks for a requester claim".into(),
        ));
    }
    let (sub, via) = match (req.event_id.is_empty(), req.run_id.is_empty()) {
        (false, true) => (
            requester_of_event(&app, &agent.pubkey, &req.event_id).await?,
            "chat".to_owned(),
        ),
        (true, false) => (
            requester_of_run(&app, &agent.pubkey, &req.run_id).await?,
            format!("run:{}", req.run_id),
        ),
        _ => {
            return Err(HiveError::InvalidArgument(
                "name either event_id or run_id".into(),
            ))
        }
    };
    let (claim, expires_at) = app
        .minter
        .sign_claim(&sub, app.community, &agent.pubkey, &via)?;
    Ok(Json(ClaimReply {
        claim,
        expires_at,
        requester: sub,
    }))
}

pub async fn jwks(State(app): State<App>) -> Json<serde_json::Value> {
    Json(app.minter.jwks())
}

/// The author of the message, when it asks this agent to act and nothing newer has.
async fn requester_of_event(app: &App, agent: &str, event_id: &str) -> Result<String, HiveError> {
    let refused = |why: &str| HiveError::PermissionDenied(format!("event {event_id}: {why}"));
    let event = app
        .directory
        .event_by_id(app.community, &event_id.to_ascii_lowercase())
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("event {event_id} does not exist")))?;
    if event.deleted {
        return Err(refused("it was deleted"));
    }
    if !MESSAGE_KINDS.contains(&event.kind) {
        return Err(refused("it is not a chat message"));
    }
    if !event.mentions(agent) {
        return Err(refused("it does not mention you"));
    }
    if app.minter.now() - event.created_at > EVENT_MAX_AGE_SECS {
        return Err(refused("it is too old to name a requester"));
    }
    let root = event.thread_root().to_owned();
    if app
        .directory
        .newer_mention(app.community, &event, &root, agent)
        .await?
    {
        return Err(refused("a newer message in its thread mentions you"));
    }
    // A stage task is posted by Beehive on a person's behalf: the person is whoever started
    // the run, and the run must have handed this stage to this agent.
    if !app.hive_pubkey.is_empty() && event.author == app.hive_pubkey {
        return requester_of_stage_task(app, agent, &event, &root).await;
    }
    Ok(event.author)
}

async fn requester_of_stage_task(
    app: &App,
    agent: &str,
    event: &RelayEvent,
    root: &str,
) -> Result<String, HiveError> {
    let refused = |why: &str| HiveError::PermissionDenied(format!("event {}: {why}", event.id));
    let mut tx = app.store.begin(app.community).await?;
    let thread = tx
        .runs()
        .thread_by_root(root)
        .await?
        .ok_or_else(|| refused("it is not in a stage thread"))?;
    if thread.agent_pubkey != agent {
        return Err(refused("the stage belongs to another agent"));
    }
    let run = tx
        .runs()
        .list(Some(thread.card_id))
        .await?
        .into_iter()
        .find(|r| {
            r.ended_at.is_none()
                && r.current_stage.as_deref() == Some(thread.stage.as_str())
                && r.visit == thread.visit
        })
        .ok_or_else(|| refused("no run is at that stage"))?;
    run.requester
        .ok_or_else(|| refused("the run names no person"))
}

/// The person who started the run, when this agent holds its current stage.
async fn requester_of_run(app: &App, agent: &str, run_id: &str) -> Result<String, HiveError> {
    let refused = |why: &str| HiveError::PermissionDenied(format!("run {run_id}: {why}"));
    let id = uuid::Uuid::parse_str(run_id)
        .map_err(|_| HiveError::InvalidArgument("run_id is not a run id".into()))?;
    let mut tx = app.store.begin(app.community).await?;
    let run = tx
        .runs()
        .get(id)
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("run {run_id} does not exist")))?;
    if run.ended_at.is_some() {
        return Err(refused("it has ended"));
    }
    let stage = run
        .current_stage
        .as_deref()
        .ok_or_else(|| refused("it has not entered a stage"))?;
    let thread = tx
        .runs()
        .thread(run.card_id, stage, run.visit)
        .await?
        .ok_or_else(|| refused("its stage has no agent yet"))?;
    if thread.agent_pubkey != agent {
        return Err(refused("its current stage belongs to another agent"));
    }
    run.requester.ok_or_else(|| refused("it names no person"))
}
