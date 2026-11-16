//! The assistant: jobs a designated Buzz agent does for a person.
//!
//! A person asks for a chat turn, a workflow edit or a skill adaptation. Beehive stores a
//! `running` job, mentions the agent named by the `assistant_agent` setting in the update
//! channel, and answers with the job's id. The agent works in its own Buzz session and reports
//! once through `assistant_result`, which ends the job and appends one `job.result` event the
//! person's poll receives. A workflow the agent reports is stored as a proposal for a person
//! to apply at their own level, so the agent never writes a package.
//!
//! The first job's root event names the thread, and a later chat turn is a new job on that
//! root. One job runs in a thread at a time. Beehive posts straight through the relay port: the
//! outbox is keyed to a card, and a job has none.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::access::ActorKind;
use crate::config::KEY_ASSISTANT_AGENT;
use crate::domain::events::{Actor, Event};
use crate::domain::workflow::{Duration as SettingDuration, Finding};
use crate::orchestrator::run::people_only;
use crate::orchestrator::save::{check_draft, draft_package, proposal_body, PROPOSAL_WORKFLOW};
use crate::orchestrator::settings::{check_channel, get_settings, update_channel, Scope};
use crate::orchestrator::task_message::MAX_POST_BYTES;
use crate::orchestrator::{Ctx, Env, OpError};
use crate::ports::relay::{Delivery, Relay, RelayError};
use crate::ports::store::{CommunityId, Job, JobState, Store, StoreError};
use crate::tool_names::{ASSISTANT_RESULT, GET_WORKFLOW};
use crate::workflow::check::has_errors;

/// The most bytes of a person's words in one request.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// The most bytes of an agent's message in a result.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// The most bytes of a draft's files put into the agent's message. A larger draft is named, and
/// the agent reads it with [`GET_WORKFLOW`].
const DRAFT_INLINE_BYTES: usize = 32 * 1024;
/// The most findings a `job.result` event carries. The rest are in the stored proposal's check.
const EVENT_FINDINGS: usize = 20;
/// Jobs ended per sweep.
pub const SWEEP_BATCH: i64 = 50;

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Chat,
    EditWorkflow,
    AdaptSkills,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::EditWorkflow => "propose_workflow_edit",
            Self::AdaptSkills => "adapt_skills",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Chat, Self::EditWorkflow, Self::AdaptSkills]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }

    /// Whether the agent must report a workflow.
    fn needs_draft(self) -> bool {
        self != Self::Chat
    }
}

/// A workflow as the person has it open: its name and every file as text.
#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    pub files: BTreeMap<String, String>,
}

/// A request for a job.
pub struct Start {
    pub kind: JobKind,
    /// The person's words.
    pub text: String,
    /// The workflow the request is about. Required for an edit or an adaptation.
    pub draft: Option<Draft>,
    /// The skills to adapt.
    pub skills: Vec<String>,
    /// The thread of an earlier job, for a later chat turn. Empty opens a new thread.
    pub root: Option<String>,
}

/// What the agent reports.
pub struct Reported {
    pub job_id: Uuid,
    /// What the agent says to the person.
    pub message: String,
    /// A workflow the agent offers, stored as a proposal.
    pub draft: Option<Draft>,
    pub base_hash: Option<String>,
}

/// A result that was recorded.
pub struct Recorded {
    pub proposal_id: Option<i64>,
    /// What the checks found in the offered workflow. None is an error.
    pub findings: Vec<Finding>,
    pub events: Vec<Event>,
}

fn relay_failed(error: RelayError) -> OpError {
    match error {
        RelayError::Unavailable(cause) => {
            OpError::Store(StoreError::Unavailable(format!("the relay: {cause}")))
        }
        RelayError::Rejected(cause) => {
            OpError::FailedPrecondition(format!("the relay refused the assistant's post: {cause}"))
        }
    }
}

/// Starts a job: stores it, mentions the assistant agent and appends `job.started`.
pub async fn start_job(
    env: &Env<'_>,
    relay: &dyn Relay,
    ctx: &Ctx,
    start: Start,
) -> Result<(Uuid, Vec<Event>), OpError> {
    let text = start.text.trim();
    if text.is_empty() {
        return Err(OpError::InvalidArgument("say what you want done".into()));
    }
    if text.len() > MAX_REQUEST_BYTES {
        return Err(OpError::InvalidArgument(format!(
            "the request is over {MAX_REQUEST_BYTES} bytes"
        )));
    }
    if start.kind.needs_draft() && start.draft.is_none() {
        return Err(OpError::InvalidArgument("give the workflow draft".into()));
    }
    if start.kind == JobKind::AdaptSkills && start.skills.is_empty() {
        return Err(OpError::InvalidArgument("name the skills to adapt".into()));
    }
    if start.root.is_some() && start.kind != JobKind::Chat {
        return Err(OpError::InvalidArgument(
            "only a chat turn continues a thread".into(),
        ));
    }
    let requester = ctx.requester_or_actor();
    let settings = get_settings(env, ctx, &Scope::User, None).await?;
    let timeout = settings
        .value("execution.timeout")
        .and_then(|v| v.as_str())
        .and_then(|text| SettingDuration::parse(text).ok())
        .and_then(|d| u64::try_from(d.nanos()).ok())
        .map(Duration::from_nanos)
        .filter(|d| !d.is_zero());

    // Everything the directory is asked comes before the first write.
    let (channel, agent) = match &start.root {
        Some(root) => {
            let mut tx = env.store.begin(ctx.community).await?;
            let prior = tx.jobs().latest_in_thread(root).await?.ok_or_else(|| {
                OpError::NotFound("there is no assistant thread with that root".into())
            })?;
            if prior.requester != requester {
                return Err(OpError::PermissionDenied(
                    "that assistant thread belongs to someone else".into(),
                ));
            }
            if prior.state == JobState::Running {
                return Err(thread_busy());
            }
            (prior.channel, prior.agent)
        }
        None => {
            let named = settings
                .value(KEY_ASSISTANT_AGENT)
                .and_then(|v| v.as_str())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    OpError::FailedPrecondition(
                        "the assistant has no agent yet: an admin sets assistant_agent to the name of a Buzz agent".into(),
                    )
                })?;
            let mut tx = env.store.begin(ctx.community).await?;
            let channel = update_channel(&mut *tx).await?.ok_or_else(|| {
                OpError::FailedPrecondition(
                    "the assistant posts in the update channel, which is not set yet".into(),
                )
            })?;
            drop(tx);
            check_channel(
                env,
                ctx,
                channel,
                "the assistant posts in the update channel: add the Beehive account to it",
            )
            .await?;
            let mut found = env.directory.agents_named(ctx.community, named).await?;
            let agent = match found.len() {
                1 => found.remove(0),
                0 => {
                    return Err(OpError::FailedPrecondition(format!(
                        "no Buzz agent is named {named}, the assistant's agent"
                    )))
                }
                n => {
                    return Err(OpError::FailedPrecondition(format!(
                        "{n} Buzz agents are named {named}; the assistant needs one"
                    )))
                }
            };
            if !env
                .directory
                .member_of_channel(ctx.community, channel, &agent.pubkey)
                .await?
            {
                return Err(OpError::FailedPrecondition(format!(
                    "the assistant's agent {named} is not in the update channel"
                )));
            }
            (channel, agent.pubkey)
        }
    };

    let mut job = Job {
        id: Uuid::new_v4(),
        kind: start.kind.as_str().to_owned(),
        requester,
        state: JobState::Running,
        channel,
        root_event_id: start.root.clone(),
        agent,
    };
    let mut tx = env.store.begin(ctx.community).await?;
    match tx.jobs().insert(&job, timeout).await {
        Ok(()) => {}
        Err(StoreError::Conflict(_)) => return Err(thread_busy()),
        Err(error) => return Err(error.into()),
    }
    tx.commit().await?;

    let message = render(&job, &start);
    let delivery = Delivery {
        key: format!("job:{}", job.id),
        at: chrono::Utc::now(),
    };
    let posted = match &start.root {
        Some(root) => {
            relay
                .post_reply(channel, root, &job.agent, &message, &delivery)
                .await
        }
        None => {
            relay
                .post_stage_task(channel, &job.agent, &message, &delivery)
                .await
        }
    };
    let root = match posted {
        Ok(root) => start.root.clone().unwrap_or(root),
        Err(error) => {
            // The agent was never asked, so the job ends here and says why to the caller.
            let mut tx = env.store.begin(ctx.community).await?;
            tx.jobs().finish(job.id, JobState::Failed, None).await?;
            tx.commit().await?;
            return Err(relay_failed(error));
        }
    };
    job.root_event_id = Some(root.clone());
    let mut tried = 0;
    let events = loop {
        tried += 1;
        match record_start(env, ctx, &job, &root, start.root.is_none()).await {
            Ok(events) => break events,
            Err(_) if tried < RECORD_TRIES => {
                tokio::time::sleep(Duration::from_millis(50 * u64::from(tried))).await;
            }
            Err(error) => {
                tracing::error!(%error, job = %job.id, %root, "could not record that a job started");
                give_up(env, relay, ctx, &job, &root).await;
                return Err(error);
            }
        }
    };
    Ok((job.id, events))
}

/// How many times the write that follows a post to the relay is tried.
const RECORD_TRIES: u32 = 3;

/// Stores the thread the agent was asked in and says the job started. The job is held first: a
/// result or an expiry that got here before the agent's mention was recorded has already ended
/// it, and `job.started` must not follow its `job.result`. The thread is stored either way, so a
/// later turn can reply in it.
async fn record_start(
    env: &Env<'_>,
    ctx: &Ctx,
    job: &Job,
    root: &str,
    opened: bool,
) -> Result<Vec<Event>, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let held = tx.jobs().get_for_update(job.id).await?;
    if opened {
        tx.jobs().set_root(job.id, root).await?;
    }
    if held.is_some_and(|held| held.state == JobState::Running) {
        tx.events()
            .append(Event::new(
                "job.started",
                None,
                None,
                ctx.actor.clone(),
                json!({
                    "job_id": job.id,
                    "kind": job.kind,
                    "requester": job.requester,
                    "root_event_id": root,
                    "agent": job.agent,
                }),
            ))
            .await?;
    }
    Ok(tx.commit().await?)
}

/// The store would not take the record of a job whose agent was already asked. Ends the job, so
/// the person can ask again and a late result is refused, and tells the agent in the thread. Both
/// are best effort: the caller is told the start failed either way.
async fn give_up(env: &Env<'_>, relay: &dyn Relay, ctx: &Ctx, job: &Job, root: &str) {
    let ended = async {
        let mut tx = env.store.begin(ctx.community).await?;
        tx.jobs().finish(job.id, JobState::Failed, None).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = ended {
        tracing::error!(%error, job = %job.id, "could not end a job that did not start");
    }
    let delivery = Delivery {
        key: format!("job:{}:cancel", job.id),
        at: chrono::Utc::now(),
    };
    let told = relay
        .post_reply(
            job.channel,
            &root.to_owned(),
            &job.agent,
            "Cancelled. Stop working on this request.",
            &delivery,
        )
        .await;
    if let Err(error) = told {
        tracing::warn!(%error, job = %job.id, "could not tell the agent a job did not start");
    }
}

fn thread_busy() -> OpError {
    OpError::FailedPrecondition(
        "the assistant is still working in that thread; wait for it, or cancel it".into(),
    )
}

/// The message that mentions the agent: the person's words, the workflow, and how to answer.
fn render(job: &Job, start: &Start) -> String {
    let mut out = format!("{}\n\n", start.text.trim());
    if let Some(draft) = &start.draft {
        let size: usize = draft.files.values().map(String::len).sum();
        if size <= DRAFT_INLINE_BYTES {
            out.push_str(&format!(
                "The workflow {:?} as the person has it open now:\n",
                draft.name
            ));
            for (path, text) in &draft.files {
                out.push_str(&format!("\n--- {path}\n{text}\n"));
            }
        } else {
            out.push_str(&format!(
                "The workflow {:?} is too long to quote: read it with {GET_WORKFLOW}.\n",
                draft.name
            ));
        }
    }
    if !start.skills.is_empty() {
        out.push_str(&format!("\nSkills to adapt: {}\n", start.skills.join(", ")));
    }
    let how = match start.kind {
        JobKind::Chat => {
            "Answer the person. Offer a workflow (name and files) only when they asked for a change."
        }
        JobKind::EditWorkflow => {
            "Make the change they asked for. Report the whole edited package: name and every file."
        }
        JobKind::AdaptSkills => {
            "Rewrite those skills to use Beehive's tools. Report the whole package: name and every file."
        }
    };
    let footer = format!(
        "\n[Beehive assistant job {id}] {how} Report once with the {ASSISTANT_RESULT} tool, jobId {id}, and a message for the person. Nothing is saved until the person applies it.",
        id = job.id
    );
    // The instructions on how to report matter most, so a long request gives way, not them.
    let mut keep = out.len().min(MAX_POST_BYTES.saturating_sub(footer.len()));
    while !out.is_char_boundary(keep) {
        keep -= 1;
    }
    out.truncate(keep);
    out.push_str(&footer);
    out
}

/// Records the agent's result: ends the job as done, stores the workflow it offered as a
/// proposal for the job's requester, and appends one `job.result` event. A workflow with an
/// error is refused with the errors and the job stays running, so the agent can fix it.
pub async fn assistant_result(
    env: &Env<'_>,
    ctx: &Ctx,
    reported: Reported,
) -> Result<Recorded, OpError> {
    let message = reported.message.trim();
    if message.is_empty() {
        return Err(OpError::InvalidArgument(
            "say something to the person in message".into(),
        ));
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(OpError::InvalidArgument(format!(
            "the message is over {MAX_MESSAGE_BYTES} bytes; say less"
        )));
    }
    let job = running_job(env, ctx, reported.job_id).await?;
    let kind = JobKind::parse(&job.kind)
        .ok_or_else(|| OpError::FailedPrecondition(format!("job {} has no known kind", job.id)))?;
    if kind.needs_draft() && reported.draft.is_none() {
        return Err(OpError::InvalidArgument(
            "report the workflow too: name and files".into(),
        ));
    }
    // The checks ask the directory, so they run before the transaction that writes.
    let offered = match reported.draft {
        None => None,
        Some(draft) => {
            let package = draft_package(
                &draft.name,
                draft
                    .files
                    .into_iter()
                    .map(|(path, text)| (path, text.into_bytes()))
                    .collect(),
            )?;
            let findings = check_draft(env, ctx, &package).await?;
            if has_errors(&findings) {
                return Err(OpError::InvalidWorkflow(findings));
            }
            Some((package, findings))
        }
    };

    let mut tx = env.store.begin(ctx.community).await?;
    // Held, so a cancel or an expiry that got here first is seen.
    let Some(held) = tx
        .jobs()
        .get_for_update(job.id)
        .await?
        .filter(|held| held.state == JobState::Running)
    else {
        return Err(ended(&job));
    };
    let (proposal_id, findings) = match offered {
        None => (None, Vec::new()),
        Some((package, findings)) => {
            let body = proposal_body(&package, reported.base_hash.as_deref(), Some(job.id));
            let id = tx
                .proposals()
                .insert(PROPOSAL_WORKFLOW, body, &job.requester)
                .await?;
            (Some(id), findings)
        }
    };
    let result = json!({
        "job_id": job.id,
        "kind": job.kind,
        "requester": job.requester,
        // The thread, for a client that did not see `job.started` before this: a result can
        // overtake it when the job is reported as soon as the agent is mentioned.
        "root_event_id": held.root_event_id,
        "state": JobState::Done.as_str(),
        "message": message,
        "proposal_id": proposal_id,
        "findings": findings.iter().take(EVENT_FINDINGS).collect::<Vec<_>>(),
    });
    tx.jobs()
        .finish(job.id, JobState::Done, Some(result.clone()))
        .await?;
    tx.events()
        .append(Event::new(
            "job.result",
            None,
            None,
            ctx.actor.clone(),
            result,
        ))
        .await?;
    let events = tx.commit().await?;
    Ok(Recorded {
        proposal_id,
        findings,
        events,
    })
}

/// The job, when it runs and the caller is its agent.
async fn running_job(env: &Env<'_>, ctx: &Ctx, id: Uuid) -> Result<Job, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let job = tx
        .jobs()
        .get_for_update(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("there is no job {id}")))?;
    if ctx.actor.kind != ActorKind::Agent || job.agent != ctx.actor.pubkey {
        return Err(OpError::PermissionDenied(
            "only the agent the job was given to may report its result".into(),
        ));
    }
    if job.state != JobState::Running {
        return Err(ended(&job));
    }
    Ok(job)
}

fn ended(job: &Job) -> OpError {
    OpError::FailedPrecondition(format!(
        "job {} already ended ({}), so its result is not taken",
        job.id,
        job.state.as_str()
    ))
}

/// Ends a running job at its requester's word. The reply so far stays, and no result event is
/// appended. The agent is told in the thread, which is best effort: the job has ended either way,
/// and a result that comes late is refused. A job that already ended is left as it is.
pub async fn cancel_job(
    env: &Env<'_>,
    relay: Option<&dyn Relay>,
    ctx: &Ctx,
    id: Uuid,
) -> Result<(), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let job = tx
        .jobs()
        .get_for_update(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("there is no job {id}")))?;
    if job.requester != ctx.requester_or_actor() {
        return Err(OpError::PermissionDenied(
            "only the person a job is for may cancel it".into(),
        ));
    }
    if job.state != JobState::Running {
        return Ok(());
    }
    tx.jobs().finish(id, JobState::Cancelled, None).await?;
    tx.commit().await?;
    if let (Some(relay), Some(root)) = (relay, &job.root_event_id) {
        let delivery = Delivery {
            key: format!("job:{id}:cancel"),
            at: chrono::Utc::now(),
        };
        let told = relay
            .post_reply(
                job.channel,
                root,
                &job.agent,
                "Cancelled. Stop working on this request.",
                &delivery,
            )
            .await;
        if let Err(error) = told {
            tracing::warn!(%error, job = %id, "could not tell the agent a job was cancelled");
        }
    }
    Ok(())
}

/// Ends the jobs whose deadline has passed as failed, each with one `job.result` event, and
/// returns the events committed. A job that cannot be ended is logged and skipped, so the jobs
/// that were ended still wake the polls waiting for them; the skipped one is tried on the next
/// pass.
pub async fn expire_jobs(
    env: &Env<'_>,
    community: CommunityId,
    limit: i64,
) -> Result<Vec<Event>, OpError> {
    let late = env
        .store
        .begin(community)
        .await?
        .jobs()
        .overdue(limit)
        .await?;
    let mut events = Vec::new();
    for job in late {
        match expire(env, community, &job).await {
            Ok(committed) => events.extend(committed),
            Err(error) => {
                tracing::error!(%error, job = %job.id, "could not end an overdue job");
            }
        }
    }
    Ok(events)
}

/// Ends one overdue job as failed. Nothing happens when a result or a cancel got here first.
async fn expire(env: &Env<'_>, community: CommunityId, job: &Job) -> Result<Vec<Event>, OpError> {
    let hive = Actor {
        pubkey: env.hive.to_owned(),
        kind: ActorKind::Agent,
    };
    let mut tx = env.store.begin(community).await?;
    let held = tx.jobs().get_for_update(job.id).await?;
    if held.is_none_or(|held| held.state != JobState::Running) {
        return Ok(Vec::new());
    }
    let result = json!({
        "job_id": job.id,
        "kind": job.kind,
        "requester": job.requester,
        "state": JobState::Failed.as_str(),
        "reason": "the assistant's agent did not report in time",
    });
    tx.jobs()
        .finish(job.id, JobState::Failed, Some(result.clone()))
        .await?;
    tx.events()
        .append(Event::new("job.result", None, None, hive, result))
        .await?;
    Ok(tx.commit().await?)
}

/// How long until the soonest deadline of a running job: zero when one has passed, `None` when
/// no running job has one.
pub async fn next_deadline_in(
    store: &dyn Store,
    community: CommunityId,
) -> Result<Option<Duration>, StoreError> {
    store.begin(community).await?.jobs().next_in().await
}

/// The caller's open proposals, oldest first. A proposal is for the person it names, so
/// nobody reads another person's.
pub async fn open_proposals(
    store: &dyn Store,
    ctx: &Ctx,
) -> Result<Vec<crate::ports::store::Proposal>, OpError> {
    people_only(ctx, "read the proposals made to a person")?;
    let mut tx = store.begin(ctx.community).await?;
    let mut open = tx.proposals().open_all().await?;
    let who = ctx.requester_or_actor();
    open.retain(|proposal| proposal.requester == who);
    Ok(open)
}

/// Takes a proposal off the person's list once they have decided it. It acts on nothing, and
/// deciding twice does nothing the second time.
pub async fn resolve_proposal(store: &dyn Store, ctx: &Ctx, id: i64) -> Result<(), OpError> {
    people_only(ctx, "decide a proposal")?;
    let mut tx = store.begin(ctx.community).await?;
    let proposal = tx
        .proposals()
        .get(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("there is no proposal {id}")))?;
    if proposal.requester != ctx.requester_or_actor() {
        return Err(OpError::PermissionDenied(
            "only the person a proposal is for may decide it".into(),
        ));
    }
    if tx.proposals().resolve(id).await? {
        tx.commit().await?;
    }
    Ok(())
}
