//! The assistant: start a job, cancel it, take the agent's result, and decide what it offered.

use std::collections::BTreeMap;

use buzz_hive_core::orchestrator::assistant::{
    assistant_result as record_result, cancel_job as cancel, open_proposals,
    resolve_proposal as resolve, start_job, Draft, JobKind, Reported, Start,
};
use buzz_hive_core::orchestrator::save::PROPOSAL_WORKFLOW;
use buzz_hive_core::ports::store::Proposal;
use uuid::Uuid;

use super::workflows::to_findings;
use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AssistantProposal, AssistantResultRequest, AssistantResultResponse, CancelJobRequest,
    CancelJobResponse, ListProposalsRequest, ListProposalsResponse, OpenProposal,
    ResolveProposalRequest, ResolveProposalResponse, StartAdaptSkillsRequest, StartChatRequest,
    StartJobResponse, StartProposeWorkflowEditRequest, WorkflowDraft,
};
use crate::unary::Ctx;

pub async fn start_chat(
    app: App,
    ctx: Ctx,
    req: StartChatRequest,
) -> Result<StartJobResponse, HiveError> {
    start(
        &app,
        &ctx,
        Start {
            kind: JobKind::Chat,
            text: req.text,
            draft: None,
            skills: Vec::new(),
            root: Some(req.root_event_id).filter(|root| !root.is_empty()),
        },
    )
    .await
}

pub async fn start_propose_workflow_edit(
    app: App,
    ctx: Ctx,
    req: StartProposeWorkflowEditRequest,
) -> Result<StartJobResponse, HiveError> {
    start(
        &app,
        &ctx,
        Start {
            kind: JobKind::EditWorkflow,
            text: req.request,
            draft: req.draft.map(draft_of),
            skills: Vec::new(),
            root: None,
        },
    )
    .await
}

pub async fn start_adapt_skills(
    app: App,
    ctx: Ctx,
    req: StartAdaptSkillsRequest,
) -> Result<StartJobResponse, HiveError> {
    let text = format!("Adapt these skills: {}.", req.skills.join(", "));
    start(
        &app,
        &ctx,
        Start {
            kind: JobKind::AdaptSkills,
            text,
            draft: req.draft.map(draft_of),
            skills: req.skills,
            root: None,
        },
    )
    .await
}

async fn start(app: &App, ctx: &Ctx, start: Start) -> Result<StartJobResponse, HiveError> {
    let relay = app.relay.as_deref().ok_or_else(no_relay)?;
    let op = app.op(true)?;
    let (id, events) = start_job(&op.env(), relay, &ctx.op(), start).await?;
    op.settle(&events);
    Ok(StartJobResponse {
        job_id: id.to_string(),
    })
}

fn no_relay() -> HiveError {
    HiveError::FailedPrecondition(
        "Beehive has no relay account yet: set BUZZ_HIVE_NOSTR_KEY and BUZZ_HIVE_RELAY_URL, then restart it"
            .into(),
    )
}

fn draft_of(draft: WorkflowDraft) -> Draft {
    Draft {
        name: draft.name,
        files: draft
            .files
            .into_iter()
            .map(|(path, bytes)| (path, String::from_utf8_lossy(&bytes).into_owned()))
            .collect(),
    }
}

fn job_id(text: &str) -> Result<Uuid, HiveError> {
    Uuid::parse_str(text).map_err(|_| HiveError::InvalidArgument("job_id is not a job id".into()))
}

pub async fn cancel_job(
    app: App,
    ctx: Ctx,
    req: CancelJobRequest,
) -> Result<CancelJobResponse, HiveError> {
    let op = app.op(false)?;
    cancel(
        &op.env(),
        app.relay.as_deref(),
        &ctx.op(),
        job_id(&req.job_id)?,
    )
    .await?;
    Ok(CancelJobResponse {})
}

pub async fn assistant_result(
    app: App,
    ctx: Ctx,
    req: AssistantResultRequest,
) -> Result<AssistantResultResponse, HiveError> {
    let draft = (!req.name.is_empty() || !req.files.is_empty()).then(|| Draft {
        name: req.name,
        files: req.files.into_iter().collect::<BTreeMap<_, _>>(),
    });
    let op = app.op(false)?;
    let recorded = record_result(
        &op.env(),
        &ctx.op(),
        Reported {
            job_id: job_id(&req.job_id)?,
            message: req.message,
            draft,
            base_hash: Some(req.base_hash).filter(|hash| !hash.is_empty()),
        },
    )
    .await?;
    op.settle(&recorded.events);
    Ok(AssistantResultResponse {
        proposal_id: recorded.proposal_id.unwrap_or_default(),
        findings: to_findings(&recorded.findings),
    })
}

pub async fn list_proposals(
    app: App,
    ctx: Ctx,
    _req: ListProposalsRequest,
) -> Result<ListProposalsResponse, HiveError> {
    let open = open_proposals(app.store.as_ref(), &ctx.op()).await?;
    Ok(ListProposalsResponse {
        proposals: open.into_iter().filter_map(open_proposal).collect(),
    })
}

/// A stored workflow proposal as the app reads it. A kind this build does not know is left out.
fn open_proposal(proposal: Proposal) -> Option<OpenProposal> {
    if proposal.kind != PROPOSAL_WORKFLOW {
        return None;
    }
    let text = |key: &str| proposal.body[key].as_str().unwrap_or_default().to_owned();
    let files = proposal.body["files"]
        .as_object()
        .map(|files| {
            files
                .iter()
                .map(|(path, body)| {
                    (
                        path.clone(),
                        body.as_str().unwrap_or_default().as_bytes().to_vec(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Some(OpenProposal {
        id: proposal.id,
        draft: Some(WorkflowDraft {
            name: text("name"),
            files,
        }),
        base_hash: text("baseHash"),
        job_id: text("jobId"),
    })
}

pub async fn resolve_proposal(
    app: App,
    ctx: Ctx,
    req: ResolveProposalRequest,
) -> Result<ResolveProposalResponse, HiveError> {
    resolve(app.store.as_ref(), &ctx.op(), req.proposal_id).await?;
    let state = if req.accepted {
        "accepted"
    } else {
        "discarded"
    };
    Ok(ResolveProposalResponse {
        proposal: Some(AssistantProposal {
            id: req.proposal_id,
            state: state.to_owned(),
            ..Default::default()
        }),
    })
}
