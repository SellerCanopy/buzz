//! What an agent uses to diagnose a failed run and report a fault, and the person's accept of a
//! ticket an agent proposed.

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::orchestrator::diagnose::{
    self, Diagnosis, Filed, Ticket, TicketKind, STUCK_AFTER_SECS,
};

use crate::app::App;
use crate::error::HiveError;
use crate::proto::assistant_proposal::Body;
use crate::proto::{
    AcceptTicketProposalRequest, AcceptTicketProposalResponse, AgentSettingIssue,
    AssistantProposal, FileTicketRequest, FileTicketResponse, GetHiveHealthRequest,
    GetHiveHealthResponse, GetRunEventsRequest, GetRunEventsResponse, ReportDiagnosisRequest,
    ReportDiagnosisResponse, StuckRun, TicketProposal,
};
use crate::routes::cards::reply;
use crate::routes::intake::issue_ref;
use crate::routes::poll::to_proto;
use crate::routes::runs::parse_id;
use crate::unary::Ctx;

pub async fn get_run_events(
    app: App,
    ctx: Ctx,
    req: GetRunEventsRequest,
) -> Result<GetRunEventsResponse, HiveError> {
    let run = parse_id(&req.run_id, "run_id")?;
    let events = diagnose::run_events(
        app.store.as_ref(),
        &ctx.op(),
        run,
        req.after_seq,
        i64::from(req.limit),
    )
    .await?;
    let next_seq = events.last().map_or(req.after_seq, |event| event.seq);
    Ok(GetRunEventsResponse {
        events: events.into_iter().map(to_proto).collect(),
        next_seq,
    })
}

pub async fn get_hive_health(
    app: App,
    ctx: Ctx,
    _req: GetHiveHealthRequest,
) -> Result<GetHiveHealthResponse, HiveError> {
    let health = diagnose::health(app.store.as_ref(), &ctx.op(), chrono::Utc::now()).await?;
    let (allowlist_applied, issues) =
        agent_tools::settings_check(app.store.as_ref(), app.supervisor.as_deref(), &ctx.op())
            .await?;
    Ok(GetHiveHealthResponse {
        head_seq: health.head_seq,
        stuck_runs: health
            .stuck_runs
            .into_iter()
            .map(|run| StuckRun {
                run_id: run.run_id.to_string(),
                card_id: run.card_id.to_string(),
                stage_id: run.stage,
                seconds: run.seconds,
            })
            .collect(),
        stuck_after_seconds: STUCK_AFTER_SECS,
        outbox_pending: health.outbox_pending,
        outbox_failed: health.outbox_failed,
        oldest_pending_seconds: health.oldest_pending_seconds,
        allowlist_applied,
        agent_issues: issues
            .into_iter()
            .map(|issue| AgentSettingIssue {
                agent: issue.agent,
                session_policy: issue.session_policy,
                allowlisted: issue.allowlisted,
                problem: issue.problem,
            })
            .collect(),
    })
}

pub async fn report_diagnosis(
    app: App,
    ctx: Ctx,
    req: ReportDiagnosisRequest,
) -> Result<ReportDiagnosisResponse, HiveError> {
    let run = parse_id(&req.run_id, "run_id")?;
    let given = req
        .diagnosis
        .ok_or_else(|| HiveError::InvalidArgument("diagnosis is required".into()))?;
    let event = diagnose::report_diagnosis(
        app.store.as_ref(),
        &ctx.op(),
        run,
        Diagnosis {
            cause: given.cause,
            stage: given.stage_id,
            evidence: given
                .evidence
                .into_iter()
                .map(|e| (e.source, e.quote))
                .collect(),
            gaps: given.gaps,
        },
    )
    .await?;
    app.op(false)?.settle(std::slice::from_ref(&event));
    Ok(ReportDiagnosisResponse {
        event_seq: event.seq,
    })
}

pub async fn file_ticket(
    app: App,
    ctx: Ctx,
    req: FileTicketRequest,
) -> Result<FileTicketResponse, HiveError> {
    let kind = TicketKind::parse(&req.kind).ok_or_else(|| {
        HiveError::InvalidArgument(format!(
            "kind is {:?}; it must be engine, ui or code",
            req.kind
        ))
    })?;
    let card = (!req.card_id.is_empty())
        .then(|| parse_id(&req.card_id, "card_id"))
        .transpose()?;
    let (filed, events) = diagnose::file_ticket(
        app.store.as_ref(),
        app.code_host.as_deref(),
        &ctx.op(),
        Ticket {
            kind,
            title: req.title,
            body: req.body,
            repos: req.repos,
            card,
        },
    )
    .await?;
    app.op(false)?.settle(&events);
    Ok(match filed {
        Filed::Created(card) => FileTicketResponse {
            outcome: "created".into(),
            card_id: card.id.to_string(),
            ..Default::default()
        },
        Filed::Noted(card) => FileTicketResponse {
            outcome: "noted".into(),
            card_id: card.id.to_string(),
            ..Default::default()
        },
        Filed::Commented { repo, number } => FileTicketResponse {
            outcome: "commented".into(),
            issue: Some(issue_ref(
                &buzz_hive_core::domain::intake::RepoRef::parse(&repo)
                    .map_err(HiveError::InvalidArgument)?,
                number,
            )),
            ..Default::default()
        },
        Filed::Proposed(id) => FileTicketResponse {
            outcome: "proposed".into(),
            proposal_id: id,
            ..Default::default()
        },
    })
}

/// A person accepts a ticket an agent proposed. An agent never does: it would approve its own.
pub async fn accept_ticket_proposal(
    app: App,
    ctx: Ctx,
    req: AcceptTicketProposalRequest,
) -> Result<AcceptTicketProposalResponse, HiveError> {
    if ctx.actor.kind == ActorKind::Agent {
        return Err(HiveError::PermissionDenied(
            "an agent may not accept a ticket; a person decides it".into(),
        ));
    }
    let host = app.require_host()?;
    let op = app.op(false)?;
    let (accepted, events) = diagnose::accept_ticket(
        &op.env(),
        host,
        &ctx.op(),
        req.proposal_id,
        &req.title,
        &req.body,
        chrono::Utc::now(),
    )
    .await?;
    op.settle(&events);
    let card = reply(&app, &ctx, accepted.card).await?;
    Ok(AcceptTicketProposalResponse {
        card: Some(card),
        proposal: Some(AssistantProposal {
            id: req.proposal_id,
            state: "accepted".to_owned(),
            body: Some(Body::Ticket(TicketProposal {
                title: accepted.title,
                body: accepted.body,
                repo: accepted.repo,
                ..Default::default()
            })),
            ..Default::default()
        }),
    })
}
