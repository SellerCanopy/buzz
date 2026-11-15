//! Sending the queued posts to the relay.
//!
//! A post is written in the transaction that causes it and sent here, so a crash between
//! the two replays the post. The relay sees one post per key: an attempt that failed, or
//! that succeeded just before a crash, is signed with the same time and content, so it is
//! the same relay event.

use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

use super::{pull_request, Env};
use crate::access::ActorKind;
use crate::domain::events::{Actor, Event};
use crate::domain::intake::RepoRef;
use crate::ports::code_host::{CodeHost, HostError, NewPullRequest};
use crate::ports::relay::{Delivery, Relay, RelayError};
use crate::ports::store::{OutboxRow, Posting, StageThread, StoreError};
use crate::workflow::source::CatalogSource;

/// How long a drainer owns a post it claimed. It is longer than the relay adapter's
/// 60 second send limit. A drainer that dies mid-send loses the post after this long, and
/// the next claim sends it again as the same relay event.
pub const CLAIM_LEASE: Duration = Duration::from_secs(90);

/// What one pass over the outbox did.
#[derive(Debug, Default)]
pub struct Drained {
    /// Posts the relay accepted.
    pub posted: usize,
    /// Posts that failed, with the cause of the first failure.
    pub failed: usize,
    pub first_error: Option<String>,
    /// The cards whose post failed in this pass. Their later posts were left queued.
    pub failed_cards: Vec<Uuid>,
    /// Events committed by the pass, for the caller to publish.
    pub events: Vec<Event>,
}

/// Sends every post that is not done, oldest first, except those of the cards in `skip`.
///
/// A post is claimed in one short transaction, sent with no transaction open, and recorded
/// in a second. So the relay's round trip holds no row lock and no pool connection, and a
/// crash between the send and the record replays the post. Only a card's oldest pending post
/// is ever claimed, so posts of one card go in order even with two drainers. A card whose
/// post failed is left for the next pass, so its later posts do not overtake it.
///
/// A failed post is kept with its attempt count and cause. The caller decides when to try
/// again, so it can back off. `stop` is checked between posts.
pub async fn drain_outbox(
    env: &Env<'_>,
    community: Uuid,
    relay: &dyn Relay,
    skip: &[Uuid],
    stop: &watch::Receiver<bool>,
) -> Result<Drained, StoreError> {
    drain_outbox_with(env, community, relay, None, skip, stop).await
}

/// What the drain needs to run the calls of action stages that sit in the outbox.
pub struct Actions<'a> {
    /// The code host, or `None` when the service has no token: the stage then fails and says so.
    pub host: Option<&'a dyn CodeHost>,
    /// Read when a call is made, so a pass with none reads no workflow from disk.
    pub catalog: &'a dyn CatalogSource,
}

/// [`drain_outbox`] that also makes the code host calls in the outbox: a pull request is opened
/// through `actions.host`, and the stage that asked for it reports the result. A host that is
/// busy or down leaves the call queued, and it fails and backs off like a post the relay
/// refused. Without `actions` a call stays queued.
pub async fn drain_outbox_with(
    env: &Env<'_>,
    community: Uuid,
    relay: &dyn Relay,
    actions: Option<&Actions<'_>>,
    skip: &[Uuid],
    stop: &watch::Receiver<bool>,
) -> Result<Drained, StoreError> {
    let mut drained = Drained::default();
    let mut blocked: Vec<Uuid> = skip.to_vec();
    loop {
        if *stop.borrow() {
            return Ok(drained);
        }
        let mut tx = env.store.begin(community).await?;
        let Some(row) = tx.outbox().claim_next(&blocked, CLAIM_LEASE).await? else {
            return Ok(drained);
        };
        // A reply goes in the thread its stage's task opened. The task was queued first, and a
        // card's posts go in order, so the thread is there unless the task failed.
        let root = match &row.posting {
            Posting::StageReply { stage, visit, .. } => tx
                .runs()
                .thread(row.card_id, stage, *visit)
                .await?
                .map(|thread| thread.root_event_id),
            _ => None,
        };
        tx.commit().await?;

        let sent = if matches!(row.posting, Posting::PullRequest { .. }) {
            open_pull_request(env, community, actions, &row)
                .await
                .map(|events| {
                    drained.events.extend(events);
                    drained.posted += 1;
                })
        } else {
            match send(relay, &row, root).await {
                Ok(root) => {
                    if record_sent(env, community, &row, &root, &mut drained).await? {
                        drained.posted += 1;
                    }
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        };
        if let Err(cause) = sent {
            let mut tx = env.store.begin(community).await?;
            match tx.outbox().mark_failed(row.id, &cause).await {
                Ok(()) | Err(StoreError::NotFound(_)) => {}
                Err(other) => return Err(other),
            }
            tx.commit().await?;
            blocked.push(row.card_id);
            drained.failed += 1;
            drained.failed_cards.push(row.card_id);
            drained.first_error.get_or_insert(cause);
        }
    }
}

/// Makes the call of `row`, a pull request, and reports its stage's result. `Err` is the cause
/// of a failure to try again: the host is busy or down, or the stage could not be told. A host
/// that refuses the call, or none at all, is the stage's failure, not the drain's.
async fn open_pull_request(
    env: &Env<'_>,
    community: Uuid,
    actions: Option<&Actions<'_>>,
    row: &OutboxRow,
) -> Result<Vec<Event>, String> {
    let Some(actions) = actions else {
        return Err("this drain does not run action stages".into());
    };
    let (host, catalog) = (actions.host, actions.catalog.load());
    let env = &Env {
        catalog: &catalog,
        ..*env
    };
    let Posting::PullRequest {
        repo,
        head,
        base,
        title,
        body,
        draft,
        ..
    } = &row.posting
    else {
        return Err("the post is not a pull request".into());
    };
    let wanted = pull_request::wanted(env, community, row)
        .await
        .map_err(|error| error.to_string())?;
    let outcome = match (host, RepoRef::parse(repo)) {
        _ if !wanted => Err("the run no longer waits for the pull request".to_owned()),
        (_, Err(reason)) => Err(reason),
        (None, _) => Err("no code host is set up; set GH_TOKEN for buzz-hive".to_owned()),
        (Some(host), Ok(repo)) => {
            let request = NewPullRequest {
                repo,
                head: head.clone(),
                base: base.clone(),
                title: title.clone(),
                body: body.clone(),
                draft: *draft,
            };
            match host.open_pull_request(&request, &row.key).await {
                Ok(pull) => Ok(pull),
                Err(error @ (HostError::RateLimited { .. } | HostError::Other(_))) => {
                    return Err(error.to_string())
                }
                Err(error) => Err(error.to_string()),
            }
        }
    };
    pull_request::finish(env, community, row, outcome)
        .await
        .map_err(|error| error.to_string())
}

/// Records a post the relay accepted: marks it done and, for a stage task, keeps the thread
/// it opened. Returns false when the card was deleted while the post was in flight, because
/// there is nothing left to record.
async fn record_sent(
    env: &Env<'_>,
    community: Uuid,
    row: &OutboxRow,
    root: &str,
    drained: &mut Drained,
) -> Result<bool, StoreError> {
    let mut tx = env.store.begin(community).await?;
    if let Posting::StageTask {
        channel,
        stage,
        visit,
        agent,
        ..
    } = &row.posting
    {
        // The thread goes first: its foreign key takes the card row, so a delete of the card
        // either waits for this transaction or has already finished and the insert is refused.
        let thread = StageThread {
            card_id: row.card_id,
            stage: stage.clone(),
            visit: *visit,
            root_event_id: root.to_owned(),
            agent_pubkey: agent.clone(),
            channel_id: *channel,
        };
        match tx.runs().record_thread(&thread).await {
            Ok(()) => {}
            Err(StoreError::Conflict(_) | StoreError::NotFound(_)) => return Ok(false),
            Err(other) => return Err(other),
        }
        match tx.outbox().mark_done(row.id).await {
            Ok(()) => {}
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(other) => return Err(other),
        }
        let event = Event::new(
            "stage.posted",
            Some(row.card_id),
            None,
            Actor {
                pubkey: env.hive.to_owned(),
                kind: ActorKind::Agent,
            },
            json!({ "stage": stage, "visit": visit, "root_event_id": root, "agent": agent }),
        );
        tx.events().append(event).await?;
    } else {
        match tx.outbox().mark_done(row.id).await {
            Ok(()) => {}
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(other) => return Err(other),
        }
    }
    drained.events.extend(tx.commit().await?);
    Ok(true)
}

async fn send(
    relay: &dyn Relay,
    row: &OutboxRow,
    root: Option<String>,
) -> Result<String, RelayError> {
    let delivery = Delivery {
        key: row.key.clone(),
        at: row.created_at,
    };
    match &row.posting {
        Posting::Status { channel, text } => relay.post_status(*channel, text, &delivery).await,
        Posting::StageTask {
            channel,
            agent,
            text,
            ..
        } => {
            relay
                .post_stage_task(*channel, agent, text, &delivery)
                .await
        }
        Posting::PullRequest { .. } => Err(RelayError::Unavailable(
            "a pull request is not a post for the relay".into(),
        )),
        Posting::StageReply {
            channel,
            agent,
            text,
            ..
        } => {
            let root = root.ok_or_else(|| {
                RelayError::Unavailable("the thread this reply goes in is not open yet".into())
            })?;
            relay
                .post_reply(*channel, &root, agent, text, &delivery)
                .await
        }
    }
}
