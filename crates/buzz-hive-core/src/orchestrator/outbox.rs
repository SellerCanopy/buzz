//! Sending the queued posts to the relay.
//!
//! A post is written in the transaction that causes it and sent here, so a crash between
//! the two replays the post. The relay sees one post per key: an attempt that failed, or
//! that succeeded just before a crash, is signed with the same time and content, so it is
//! the same relay event.

use serde_json::json;
use uuid::Uuid;

use super::Env;
use crate::access::ActorKind;
use crate::domain::events::{Actor, Event};
use crate::ports::relay::{Delivery, Relay, RelayError};
use crate::ports::store::{OutboxRow, Posting, StageThread, StoreError};

/// What one pass over the outbox did.
#[derive(Debug, Default)]
pub struct Drained {
    /// Posts the relay accepted.
    pub posted: usize,
    /// Posts that failed, with the cause of the first failure.
    pub failed: usize,
    pub first_error: Option<String>,
    /// Events committed by the pass, for the caller to publish.
    pub events: Vec<Event>,
}

/// Sends every post that is not done, oldest first. One transaction per post, so a failure
/// loses nothing and two drainers never take the same row. A card whose post failed is
/// left for the next pass, so its later posts do not overtake it.
///
/// A failed post is kept with its attempt count and cause. The caller decides when to try
/// again, so it can back off.
pub async fn drain_outbox(
    env: &Env<'_>,
    community: Uuid,
    relay: &dyn Relay,
) -> Result<Drained, StoreError> {
    let mut drained = Drained::default();
    let mut blocked: Vec<Uuid> = Vec::new();
    loop {
        let mut tx = env.store.begin(community).await?;
        let Some(row) = tx.outbox().next(&blocked).await? else {
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
        match send(relay, &row, root).await {
            Ok(root) => {
                tx.outbox().mark_done(row.id).await?;
                if let Posting::StageTask {
                    stage,
                    visit,
                    agent,
                    ..
                } = &row.posting
                {
                    tx.runs()
                        .record_thread(&StageThread {
                            card_id: row.card_id,
                            stage: stage.clone(),
                            visit: *visit,
                            root_event_id: root.clone(),
                            agent_pubkey: agent.clone(),
                        })
                        .await?;
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
                }
                drained.events.extend(tx.commit().await?);
                drained.posted += 1;
            }
            Err(error) => {
                let cause = error.to_string();
                tx.outbox().mark_failed(row.id, &cause).await?;
                tx.commit().await?;
                blocked.push(row.card_id);
                drained.failed += 1;
                drained.first_error.get_or_insert(cause);
            }
        }
    }
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
