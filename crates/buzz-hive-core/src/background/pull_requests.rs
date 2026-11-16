//! The pull requests of cards: read them from the host, keep what was read, and let the stages
//! that wait on them pass or fail.
//!
//! Host calls per card per pass: one pull request read and, while it is open, one checks read.
//! Both are conditional requests in the adapter, so a pull request that did not change costs
//! no quota. How often a card is read is up to its caller: the loop in `buzz-hive` reads each
//! card at its waiting stage's poll, and every 60 seconds otherwise.

use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::events::Event;
use crate::domain::intake::RepoRef;
use crate::domain::run::RunState;
use crate::orchestrator::intake::IntakeError;
use crate::orchestrator::wait::{record, Seen};
use crate::orchestrator::{Env, OpError};
use crate::ports::code_host::{CodeHost, HostError};
use crate::ports::store::CommunityId;
use crate::rules::wait::{waiting_of, DEFAULT_POLL};

/// What a pass over one card did.
#[derive(Debug)]
pub struct Synced {
    /// How long until the card is worth reading again.
    pub poll: Duration,
    /// The run moved: a waiting stage passed or failed, so a post may be queued.
    pub moved: bool,
    pub events: Vec<Event>,
}

/// The cards to read: those with an open pull request, and those whose run waits on outside
/// state, which may have no pull request at all.
pub async fn watched(env: &Env<'_>, community: CommunityId) -> Result<Vec<Uuid>, OpError> {
    let mut tx = env.store.begin(community).await?;
    let mut cards: Vec<Uuid> = tx
        .pull_requests()
        .open()
        .await?
        .into_iter()
        .map(|(card, _)| card)
        .collect();
    for run in tx.runs().active().await? {
        let waits = run.state == RunState::Running
            && run
                .current_stage
                .as_deref()
                .and_then(|id| {
                    env.catalog
                        .pinned(&run.workflow, &run.workflow_hash)?
                        .stage(id)
                })
                .is_some_and(|(stage, _)| waiting_of(stage).is_some());
        if waits && !cards.contains(&run.card_id) {
            cards.push(run.card_id);
        }
    }
    Ok(cards)
}

/// Reads `card`'s pull request from the host and writes what was read. `now` is the time of the
/// read.
pub async fn sync_card(
    env: &Env<'_>,
    host: &dyn CodeHost,
    community: CommunityId,
    card: Uuid,
    now: DateTime<Utc>,
) -> Result<Synced, IntakeError> {
    let (pull, poll) = {
        let mut tx = env.store.begin(community).await?;
        let pull = tx.pull_requests().by_card(card).await?;
        let waiting = match tx.runs().active_for_card(card).await? {
            Some(run) if run.state == RunState::Running => run
                .current_stage
                .as_deref()
                .and_then(|id| {
                    env.catalog
                        .pinned(&run.workflow, &run.workflow_hash)?
                        .stage(id)
                })
                .and_then(|(stage, _)| waiting_of(stage)),
            _ => None,
        };
        (pull, waiting.map_or(DEFAULT_POLL, |w| w.poll))
    };
    let mut seen = Seen {
        status: None,
        checks: None,
        head_sha: None,
        gone: false,
        now,
    };
    if let Some(pull) = &pull {
        let repo = RepoRef::parse(&pull.repo).map_err(OpError::FailedPrecondition)?;
        match host.get_pull_request(&repo, pull.number).await {
            Ok(status) => {
                if status.pull.state == "open" {
                    let read = host.list_checks(&repo, pull.number).await?;
                    seen.checks = Some(read.checks);
                    seen.head_sha = Some(read.head_sha);
                }
                seen.status = Some(status);
            }
            Err(HostError::NotFound(_)) => seen.gone = true,
            Err(other) => return Err(other.into()),
        }
    }
    let (events, moved) = record(env, community, card, &seen).await?;
    Ok(Synced {
        poll,
        moved,
        events,
    })
}
