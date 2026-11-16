//! What an agent needs to diagnose a failed run and report a fault: a run's events, the engine's
//! health, a recorded diagnosis, and a ticket for the fault.
//!
//! An engine or UI fault becomes a Backlog card. A fault in a card's own code waits as a ticket
//! proposal, and a person's accept opens the GitHub issue and imports it. Both first look for
//! an open card or issue that says the same, and add to it instead of opening a second.

use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::cards::{add_note, insert_card, NewCard, MAX_NOTE_DETAIL_CHARS, MAX_TITLE_CHARS};
use super::intake::{card_from_issue, IntakeError};
use super::repos::repos_of;
use super::{Ctx, Env, OpError};
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::domain::intake::{IssueFilter, Page, RepoRef};
use crate::domain::run::RunState;
use crate::ports::code_host::{CodeHost, HostError};
use crate::ports::store::{Store, StoreError, Tx};

/// `run_events` returns this many events when the caller names no limit.
pub const DEFAULT_EVENTS_LIMIT: i64 = 100;
/// The most events `run_events` returns.
pub const MAX_EVENTS_LIMIT: i64 = 500;
/// A running stage older than this many seconds is reported as stuck.
pub const STUCK_AFTER_SECS: i64 = 3600;
/// The longest ticket body, in characters, so a note made from it fits.
pub const MAX_TICKET_BODY_CHARS: usize = 8000;
/// The issues of a repo read when looking for a duplicate.
const DUPLICATE_SCAN: u32 = 100;
/// The proposal kind a code fault waits as.
pub const PROPOSAL_TICKET: &str = "ticket";

const MAX_CAUSE_CHARS: usize = 2000;
const MAX_QUOTE_CHARS: usize = 1000;
const MAX_SOURCE_CHARS: usize = 200;
const MAX_EVIDENCE: usize = 20;
const MAX_GAPS: usize = 10;
const MAX_GAP_CHARS: usize = 300;

/// One run's events after `after_seq`, oldest first. `limit` 0 means the default.
pub async fn run_events(
    store: &dyn Store,
    ctx: &Ctx,
    run_id: Uuid,
    after_seq: i64,
    limit: i64,
) -> Result<Vec<Event>, OpError> {
    if !(0..=MAX_EVENTS_LIMIT).contains(&limit) {
        return Err(OpError::InvalidArgument(format!(
            "limit is {limit}; it must be 1 to {MAX_EVENTS_LIMIT}, or 0 for {DEFAULT_EVENTS_LIMIT}"
        )));
    }
    let limit = if limit == 0 {
        DEFAULT_EVENTS_LIMIT
    } else {
        limit
    };
    let mut tx = store.begin(ctx.community).await?;
    let run = tx
        .runs()
        .get(run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {run_id} does not exist")))?;
    Ok(tx
        .events()
        .of_run(run.card_id, run_id, after_seq.max(0), limit)
        .await?)
}

/// A run that has sat in one stage too long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckRun {
    pub run_id: Uuid,
    pub card_id: Uuid,
    pub stage: String,
    pub seconds: i64,
}

/// What `health` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub head_seq: i64,
    pub stuck_runs: Vec<StuckRun>,
    pub outbox_pending: i64,
    pub outbox_failed: i64,
    /// How long the oldest unsent post has waited, 0 when none waits.
    pub oldest_pending_seconds: i64,
}

/// The engine's own state, read in one transaction. A run is stuck when it is running, not
/// waiting for a person or paused, and its stage was entered over [`STUCK_AFTER_SECS`] ago.
pub async fn health(store: &dyn Store, ctx: &Ctx, now: DateTime<Utc>) -> Result<Health, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let head_seq = tx.events().latest_seq().await?;
    let backlog = tx.outbox().backlog().await?;
    let stuck_runs = tx
        .runs()
        .active()
        .await?
        .into_iter()
        .filter(|run| run.state == RunState::Running)
        .filter_map(|run| {
            let stage = run.current_stage.clone()?;
            let since = run
                .current_step()
                .and_then(|step| step.entered_at)
                .unwrap_or(run.started_at);
            let seconds = (now - since).num_seconds();
            (seconds > STUCK_AFTER_SECS).then_some(StuckRun {
                run_id: run.id,
                card_id: run.card_id,
                stage,
                seconds,
            })
        })
        .collect();
    Ok(Health {
        head_seq,
        stuck_runs,
        outbox_pending: backlog.pending,
        outbox_failed: backlog.failed,
        oldest_pending_seconds: backlog
            .oldest
            .map_or(0, |at| (now - at).num_seconds().max(0)),
    })
}

/// An agent's diagnosis of a run.
#[derive(Debug, Clone, Default)]
pub struct Diagnosis {
    /// One sentence naming the likely cause.
    pub cause: String,
    /// The stage the fault is in, empty when the agent cannot say.
    pub stage: String,
    /// `(source, quote)` pairs.
    pub evidence: Vec<(String, String)>,
    /// What the agent could not see.
    pub gaps: Vec<String>,
}

/// Records the diagnosis in the run's card activity (`activity.added`, type `diagnosis`). It
/// needs at least one evidence quote: a cause nobody can check is not a diagnosis.
pub async fn report_diagnosis(
    store: &dyn Store,
    ctx: &Ctx,
    run_id: Uuid,
    diagnosis: Diagnosis,
) -> Result<Event, OpError> {
    let too_long = |what: &str, max: usize| {
        OpError::InvalidArgument(format!("{what} must be at most {max} characters"))
    };
    let cause = diagnosis.cause.trim();
    if cause.is_empty() || cause.chars().count() > MAX_CAUSE_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "cause must be 1 to {MAX_CAUSE_CHARS} characters"
        )));
    }
    let evidence: Vec<(&str, &str)> = diagnosis
        .evidence
        .iter()
        .map(|(source, quote)| (source.trim(), quote.trim()))
        .collect();
    if !evidence.iter().any(|(_, quote)| !quote.is_empty()) {
        return Err(OpError::InvalidArgument(
            "a diagnosis needs at least one evidence quote".into(),
        ));
    }
    if evidence.len() > MAX_EVIDENCE {
        return Err(OpError::InvalidArgument(format!(
            "name at most {MAX_EVIDENCE} pieces of evidence"
        )));
    }
    for (source, quote) in &evidence {
        if quote.chars().count() > MAX_QUOTE_CHARS {
            return Err(too_long("an evidence quote", MAX_QUOTE_CHARS));
        }
        if source.chars().count() > MAX_SOURCE_CHARS {
            return Err(too_long("an evidence source", MAX_SOURCE_CHARS));
        }
    }
    if diagnosis.gaps.len() > MAX_GAPS
        || diagnosis
            .gaps
            .iter()
            .any(|g| g.chars().count() > MAX_GAP_CHARS)
    {
        return Err(OpError::InvalidArgument(format!(
            "name at most {MAX_GAPS} gaps of {MAX_GAP_CHARS} characters"
        )));
    }
    let mut tx = store.begin(ctx.community).await?;
    let run = tx
        .runs()
        .get(run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {run_id} does not exist")))?;
    let stage = diagnosis.stage.trim();
    if !stage.is_empty() && !run.steps.iter().any(|step| step.stage == stage) {
        return Err(OpError::InvalidArgument(format!(
            "run {run_id} never entered stage {stage}"
        )));
    }
    let summary: String = cause
        .chars()
        .take(super::activity::MAX_SUMMARY_CHARS)
        .collect::<String>()
        .replace('\n', " ");
    tx.events()
        .append(Event::new(
            "activity.added",
            Some(run.card_id),
            Some(run_id),
            ctx.actor.clone(),
            json!({
                "type": "diagnosis",
                "summary": summary,
                "detail": cause,
                "stage": stage,
                "evidence": evidence
                    .iter()
                    .filter(|(_, quote)| !quote.is_empty())
                    .map(|(source, quote)| json!({ "source": source, "quote": quote }))
                    .collect::<Vec<_>>(),
                "gaps": diagnosis.gaps,
            }),
        ))
        .await?;
    let mut events = tx.commit().await?;
    events
        .pop()
        .ok_or_else(|| OpError::FailedPrecondition("the diagnosis was not saved".into()))
}

/// Where a fault is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketKind {
    /// Beehive itself.
    Engine,
    /// The app's screens.
    Ui,
    /// The code of the card the agent worked on.
    Code,
}

impl TicketKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "engine" => Some(Self::Engine),
            "ui" => Some(Self::Ui),
            "code" => Some(Self::Code),
            _ => None,
        }
    }
}

/// A fault an agent reports.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub kind: TicketKind,
    pub title: String,
    pub body: String,
    /// `owner/name` of the repos it concerns. A code fault names one on the card, or none for
    /// the card's first repo.
    pub repos: Vec<String>,
    /// The card whose code is at fault. Required for a code fault.
    pub card: Option<Uuid>,
}

/// What `file_ticket` did.
#[derive(Debug, Clone, PartialEq)]
pub enum Filed {
    /// A Backlog card was made.
    Created(Card),
    /// An open card already had the fault and got a note.
    Noted(Card),
    /// An open issue already had the fault and got a comment.
    Commented { repo: String, number: u64 },
    /// A code fault waits for a person to accept it.
    Proposed(i64),
}

/// The title as it is compared for a duplicate: case and runs of spaces do not matter.
fn same_title(a: &str, b: &str) -> bool {
    let words =
        |text: &str| -> Vec<String> { text.split_whitespace().map(str::to_lowercase).collect() };
    words(a) == words(b)
}

fn missing_host() -> OpError {
    OpError::FailedPrecondition(
        "Beehive has no GitHub token yet: set GH_TOKEN, then restart it".into(),
    )
}

/// The first repo of `repos`' card that matches, as `(repo, number)`, among the open issues.
async fn find_issue(
    host: &dyn CodeHost,
    repos: &[RepoRef],
    title: &str,
) -> Result<Option<(RepoRef, u64)>, IntakeError> {
    for repo in repos {
        let open = match host
            .list_issues(repo, &IssueFilter::default(), Page::first(DUPLICATE_SCAN))
            .await
        {
            Err(HostError::NotFound(_)) => {
                return Err(
                    OpError::NotFound(format!("{repo} does not exist or cannot be read")).into(),
                )
            }
            other => other?,
        };
        if let Some(found) = open
            .into_iter()
            .find(|issue| same_title(&issue.title, title))
        {
            return Ok(Some((repo.clone(), found.number)));
        }
    }
    Ok(None)
}

/// The open card whose title says the same as `title`.
async fn open_card_titled(tx: &mut dyn Tx, title: &str) -> Result<Option<Card>, StoreError> {
    Ok(tx
        .cards()
        .list(false)
        .await?
        .into_iter()
        .find(|card| card.state != CardState::Done && same_title(&card.title, title)))
}

/// Adds the report to the card that already has the fault.
async fn note_card(
    store: &dyn Store,
    ctx: &Ctx,
    card: Card,
    title: &str,
    body: &str,
) -> Result<(Filed, Vec<Event>), IntakeError> {
    let event = add_note(
        store,
        ctx,
        card.id,
        "Reported again",
        &note_detail(title, body),
    )
    .await?;
    Ok((Filed::Noted(card), vec![event]))
}

/// Refuses a repo that no card, issue or intake rule of the community names. Beehive reads and
/// comments on the repos it is given with the team's token, so an agent may name only the ones
/// a person has already linked.
async fn check_known(store: &dyn Store, ctx: &Ctx, repos: &[RepoRef]) -> Result<(), OpError> {
    if repos.is_empty() {
        return Ok(());
    }
    let mut tx = store.begin(ctx.community).await?;
    let mut known: Vec<String> = tx
        .intake()
        .list()
        .await?
        .into_iter()
        .map(|rule| rule.repo.to_string())
        .collect();
    known.extend(
        tx.issues()
            .attached()
            .await?
            .into_iter()
            .map(|row| row.issue.repo.to_string()),
    );
    for card in tx.cards().list(false).await? {
        known.extend(tx.card_repos().of_card(card.id).await?);
    }
    match repos.iter().find(|repo| {
        !known
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&repo.to_string()))
    }) {
        Some(repo) => Err(OpError::PermissionDenied(format!(
            "{repo} is not linked to a card or an intake rule; a person links it first"
        ))),
        None => Ok(()),
    }
}

/// Files the fault, or adds it to the open card or issue that already has it.
pub async fn file_ticket(
    store: &dyn Store,
    host: Option<&dyn CodeHost>,
    ctx: &Ctx,
    ticket: Ticket,
) -> Result<(Filed, Vec<Event>), IntakeError> {
    let title = ticket.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "title must be 1 to {MAX_TITLE_CHARS} characters"
        ))
        .into());
    }
    if ticket.body.chars().count() > MAX_TICKET_BODY_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "body must be at most {MAX_TICKET_BODY_CHARS} characters"
        ))
        .into());
    }
    let mut repos = ticket
        .repos
        .iter()
        .map(|repo| RepoRef::parse(repo).map_err(OpError::InvalidArgument))
        .collect::<Result<Vec<_>, _>>()?;
    repos.dedup();
    if repos.len() > super::repos::MAX_CARD_REPOS {
        return Err(OpError::InvalidArgument(format!(
            "name at most {} repos",
            super::repos::MAX_CARD_REPOS
        ))
        .into());
    }
    if ticket.kind == TicketKind::Code {
        let card = ticket.card.ok_or_else(|| {
            OpError::InvalidArgument("a fault in a card's code names the card".into())
        })?;
        let mut tx = store.begin(ctx.community).await?;
        tx.cards()
            .get(card)
            .await?
            .ok_or_else(|| OpError::NotFound(format!("card {card} does not exist")))?;
        let on_card = repos_of(&mut *tx, card).await?;
        drop(tx);
        let repo = match repos.as_slice() {
            [] => on_card
                .first()
                .ok_or_else(|| OpError::FailedPrecondition(format!("card {card} has no repo")))?
                .clone(),
            [one] if on_card.contains(&one.to_string()) => one.to_string(),
            [one] => {
                return Err(
                    OpError::InvalidArgument(format!("{one} is not a repo of card {card}")).into(),
                )
            }
            _ => {
                return Err(OpError::InvalidArgument(
                    "a fault in a card's code names one repo".into(),
                )
                .into())
            }
        };
        repos = vec![RepoRef::parse(&repo).map_err(OpError::InvalidArgument)?];
    }
    if ticket.kind != TicketKind::Code {
        check_known(store, ctx, &repos).await?;
    }
    // An open card that says the same.
    {
        let mut tx = store.begin(ctx.community).await?;
        if let Some(card) = open_card_titled(&mut *tx, &title).await? {
            drop(tx);
            return note_card(store, ctx, card, &title, &ticket.body).await;
        }
    }
    // An open issue that says the same. Asking GitHub is a convenience for a fault in the engine
    // or the UI, which is reported even when GitHub cannot be asked, and part of the contract for
    // a fault in a card's code.
    let found = match (repos.is_empty(), host) {
        (true, _) => Ok(None),
        (false, Some(host)) => find_issue(host, &repos, &title).await,
        (false, None) => Err(missing_host().into()),
    };
    let found = match found {
        Err(error)
            if ticket.kind != TicketKind::Code
                && !matches!(error, IntakeError::Op(OpError::NotFound(_))) =>
        {
            tracing::warn!(%error, "could not look for an open issue, so the fault is filed without the check");
            None
        }
        other => other?,
    };
    if let (Some((repo, number)), Some(host)) = (found, host) {
        let key = format!(
            "ticket:{repo}#{number}:{}",
            hex::encode(Sha256::digest(
                format!("{}\n{}", ctx.actor.pubkey, ticket.body).as_bytes()
            ))
            .get(..16)
            .unwrap_or_default()
        );
        host.comment(&repo, number, &note_detail(&title, &ticket.body), &key)
            .await?;
        return Ok((
            Filed::Commented {
                repo: repo.to_string(),
                number,
            },
            Vec::new(),
        ));
    }
    let mut tx = store.begin(ctx.community).await?;
    // Another filing of the same fault may have committed since the reads above. The log lock
    // queues this one behind it, and the lookups below see what it wrote.
    tx.events().lock_log().await?;
    if let Some(card) = open_card_titled(&mut *tx, &title).await? {
        drop(tx);
        return note_card(store, ctx, card, &title, &ticket.body).await;
    }
    if ticket.kind == TicketKind::Code {
        let repo = repos[0].to_string();
        let card = ticket.card.map(|id| id.to_string());
        // A proposal waiting for this fault is not proposed again.
        let waiting = tx
            .proposals()
            .open(PROPOSAL_TICKET)
            .await?
            .into_iter()
            .find(|p| {
                same_title(p.body["title"].as_str().unwrap_or_default(), &title)
                    && p.body["repo"].as_str() == Some(repo.as_str())
            });
        if let Some(waiting) = waiting {
            return Ok((Filed::Proposed(waiting.id), Vec::new()));
        }
        let id = tx
            .proposals()
            .insert(
                PROPOSAL_TICKET,
                json!({
                    "title": title,
                    "body": ticket.body,
                    "repo": repo,
                    "cardId": card,
                    "reportedBy": ctx.actor.pubkey,
                }),
                &ctx.requester_or_actor(),
            )
            .await?;
        tx.commit().await?;
        return Ok((Filed::Proposed(id), Vec::new()));
    }
    let card = insert_card(
        &mut *tx,
        ctx,
        NewCard {
            title,
            description: ticket.body,
            workflow: String::new(),
        },
    )
    .await?;
    for repo in &repos {
        tx.card_repos().add(card.id, &repo.to_string()).await?;
    }
    if !repos.is_empty() {
        tx.events()
            .append(Event::new(
                "card.repos",
                Some(card.id),
                None,
                ctx.actor.clone(),
                json!({ "repos": repos.iter().map(ToString::to_string).collect::<Vec<_>>() }),
            ))
            .await?;
    }
    let events = tx.commit().await?;
    Ok((Filed::Created(card), events))
}

/// A note's or comment's text: the title, then the report.
fn note_detail(title: &str, body: &str) -> String {
    format!("{title}\n\n{body}")
}

// A note made from a ticket always fits what a note holds.
const _: () = assert!(MAX_TITLE_CHARS + MAX_TICKET_BODY_CHARS + 2 <= MAX_NOTE_DETAIL_CHARS);

/// The ticket a person accepted, as it was filed.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub card: Card,
    pub title: String,
    pub body: String,
    pub repo: String,
}

/// A person accepts a code-fault ticket: the issue is opened on the card's repo, imported as a
/// Backlog card, and the proposal is closed. `title` and `body` replace the draft's when not
/// empty. Only the person the proposal is for may accept it. A repeat after a failure finds the
/// issue it opened and makes no second.
pub async fn accept_ticket(
    env: &Env<'_>,
    host: &dyn CodeHost,
    ctx: &Ctx,
    proposal_id: i64,
    title: &str,
    body: &str,
    now: DateTime<Utc>,
) -> Result<(Accepted, Vec<Event>), IntakeError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let proposal = tx
        .proposals()
        .open(PROPOSAL_TICKET)
        .await?
        .into_iter()
        .find(|p| p.id == proposal_id)
        .ok_or_else(|| {
            OpError::NotFound(format!("there is no open ticket proposal {proposal_id}"))
        })?;
    if proposal.requester != ctx.actor.pubkey {
        return Err(OpError::PermissionDenied(
            "only the person a ticket proposal is for may accept it".into(),
        )
        .into());
    }
    let text = |key: &str| proposal.body[key].as_str().unwrap_or_default().to_owned();
    let title = if title.trim().is_empty() {
        text("title")
    } else {
        title.trim().to_owned()
    };
    let body = if body.is_empty() {
        text("body")
    } else {
        body.to_owned()
    };
    if title.chars().count() > MAX_TITLE_CHARS || body.chars().count() > MAX_TICKET_BODY_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "title is at most {MAX_TITLE_CHARS} characters and body at most {MAX_TICKET_BODY_CHARS}"
        ))
        .into());
    }
    let repo_text = text("repo");
    let repo = RepoRef::parse(&repo_text).map_err(OpError::InvalidArgument)?;
    // Claim the proposal before GitHub is asked, so a double click, a second device or a discard
    // that lands first cannot open the issue twice or open it for a proposal already decided.
    // The decision is undone below when the issue cannot be opened.
    if !tx.proposals().resolve(proposal_id).await? {
        return Err(
            OpError::NotFound(format!("there is no open ticket proposal {proposal_id}")).into(),
        );
    }
    tx.commit().await?;
    let opened = async {
        let issue = host
            .create_issue(
                &repo,
                &title,
                &body,
                &format!("ticket-proposal:{proposal_id}"),
            )
            .await?;
        card_from_issue(env, ctx, host, &repo, issue.number, "", now).await
    }
    .await;
    let (card, _, events) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let mut tx = env.store.begin(ctx.community).await?;
            tx.proposals().reopen(proposal_id).await?;
            tx.commit().await?;
            return Err(error);
        }
    };
    Ok((
        Accepted {
            card,
            title,
            body,
            repo: repo_text,
        },
        events,
    ))
}
