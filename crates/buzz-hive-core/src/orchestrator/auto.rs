//! Auto mode: answering a card's Needs you items by rule, with no person.
//!
//! [`settle`] runs at the end of every step that opens an item (see `routes::apply`), and when
//! a person turns auto mode on for a card that already waits. The rules are in
//! [`crate::rules::auto`]; this module reads what they need and sends each answer down the
//! path a person's answer takes, as the Beehive account.
//!
//! An answer that cannot be carried out, for example because the next stage's agent left the
//! update channel, is undone by a savepoint and left to a person with a note in Activity, so a
//! failed auto answer never fails the step that opened the item.

use std::collections::{BTreeMap, HashSet};

use serde_json::json;
use uuid::Uuid;

use super::inbox::decide;
use super::routes::{append, auto_mode_of};
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::domain::events::Actor;
use crate::domain::inbox::{InboxItem, InboxKind, ANSWER_APPROVE, ANSWER_RETRY};
use crate::ports::store::Tx;
use crate::rules::auto::{auto_answer, AutoAnswer, Tries, AUTO_RESUMES, AUTO_RETRIES};
use crate::workflow::load::Package;

/// The kind of activity entry an auto answer leaves, beside a person's `answer`.
pub const ACTIVITY_AUTO: &str = "auto";

/// Whether the card is in auto mode.
pub async fn is_on(tx: &mut dyn Tx, card: Uuid) -> Result<bool, OpError> {
    auto_mode_of(tx, card).await
}

/// Answers the open items of a card in auto mode, as far as the rules allow. Returns whether
/// any item was answered, so the caller reads the card and its run again.
pub(super) async fn settle(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card_id: Uuid,
) -> Result<bool, OpError> {
    if !auto_mode_of(tx, card_id).await? {
        return Ok(false);
    }
    let engine = Ctx {
        community: ctx.community,
        actor: Actor {
            pubkey: env.hive.to_owned(),
            kind: ActorKind::Agent,
        },
    };
    let mut answered = false;
    for open in tx.inbox().list(Some(card_id), false).await? {
        // An earlier answer may have closed this item.
        let Some(item) = tx.inbox().get(open.id).await?.filter(InboxItem::is_open) else {
            continue;
        };
        let (answer, reason) = decide_for(env, tx, &item).await?;
        let (choice, reason) = match answer {
            AutoAnswer::Approve => (ANSWER_APPROVE, reason),
            AutoAnswer::Retry | AutoAnswer::Resume => (ANSWER_RETRY, reason),
            AutoAnswer::Person { reason } => {
                leave(tx, &engine, &item, &reason).await?;
                continue;
            }
        };
        tx.savepoint().await?;
        let tried = Box::pin(decide(
            env,
            &engine,
            tx,
            card_id,
            Some(item.id),
            choice,
            "",
            &BTreeMap::new(),
            Some(&reason),
        ))
        .await;
        match tried {
            Ok(_) => {
                tx.release_savepoint().await?;
                answered = true;
            }
            // A failed store may have left the transaction unusable: let the caller see it.
            Err(OpError::Store(e)) => return Err(e.into()),
            Err(refused) => {
                tx.rollback_savepoint().await?;
                leave(
                    tx,
                    &engine,
                    &item,
                    &format!("auto mode could not answer: {refused}"),
                )
                .await?;
            }
        }
    }
    Ok(answered)
}

/// What the rules say about `item`, and the reason to record when they answer it.
async fn decide_for(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    item: &InboxItem,
) -> Result<(AutoAnswer, String), OpError> {
    let (listed, saved, versions) = match item.kind {
        InboxKind::Approval => artifacts_of(env, tx, item).await?,
        _ => Default::default(),
    };
    let tries = tries_of(tx, item, env.hive).await?;
    let answer = auto_answer(item.kind, &tries, &listed, &saved);
    let reason = match &answer {
        AutoAnswer::Approve => format!("approved {}", versions.join(", ")),
        AutoAnswer::Retry => format!(
            "the run failed, so auto mode started it again (retry {} of {AUTO_RETRIES})",
            tries.retries + 1
        ),
        AutoAnswer::Resume => format!(
            "the stage did not report, so auto mode asked its agent to carry on (try {} of {AUTO_RESUMES})",
            tries.resumes + 1
        ),
        AutoAnswer::Person { .. } => String::new(),
    };
    Ok((answer, reason))
}

/// The artifacts an approval lists, the ones with a saved version, and each of those as
/// `name vN` for the reason.
pub(super) async fn artifacts_of(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    item: &InboxItem,
) -> Result<(Vec<String>, HashSet<String>, Vec<String>), OpError> {
    let listed = listed_by(env, tx, item.card_id, &item.stage).await?;
    let mut saved = HashSet::new();
    let mut versions = Vec::new();
    for name in &listed {
        if let Some(version) = tx.artifacts().version(item.card_id, name, None).await? {
            saved.insert(name.clone());
            versions.push(format!("{name} v{}", version.version));
        }
    }
    Ok((listed, saved, versions))
}

/// The artifacts the approval stage `stage` of the card's workflow lists.
pub(super) async fn listed_by(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card_id: Uuid,
    stage: &str,
) -> Result<Vec<String>, OpError> {
    let card = tx
        .cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let package: Option<&Package> = env.catalog.get(&card.workflow);
    Ok(package
        .and_then(|p| p.stage(stage))
        .and_then(|(stage, _)| stage.approval.as_ref())
        .map(|approval| approval.artifacts.clone())
        .unwrap_or_default())
}

/// How many times auto mode already answered this kind of item with a retry, in a row. A
/// failure counts the failures of the card, since each retry is a new run. A missing result
/// counts the ones of the same stage visit, which a retry keeps.
async fn tries_of(tx: &mut dyn Tx, item: &InboxItem, engine: &str) -> Result<Tries, OpError> {
    let same = |other: &InboxItem| {
        other.kind == item.kind
            && other.stage == item.stage
            && (item.kind != InboxKind::NoResult || other.visit == item.visit)
    };
    // Open items come first and the resolved ones newest first.
    let in_a_row = tx
        .inbox()
        .list(Some(item.card_id), true)
        .await?
        .iter()
        .filter(|other| !other.is_open() && same(other))
        .take_while(|other| {
            other.resolved_by.as_deref() == Some(engine)
                && other.answer.as_deref() == Some(ANSWER_RETRY)
        })
        .count() as u32;
    Ok(match item.kind {
        InboxKind::Failure => Tries {
            retries: in_a_row,
            resumes: 0,
        },
        InboxKind::NoResult => Tries {
            retries: 0,
            resumes: in_a_row,
        },
        _ => Tries::default(),
    })
}

/// Says in the card's activity that auto mode left `item` for a person.
async fn leave(
    tx: &mut dyn Tx,
    engine: &Ctx,
    item: &InboxItem,
    reason: &str,
) -> Result<(), OpError> {
    let run = tx
        .runs()
        .get(item.run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {} does not exist", item.run_id)))?;
    append(
        tx,
        engine,
        "activity.added",
        &run,
        json!({
            "type": "note",
            "summary": format!("auto mode left {} for a person", item.stage),
            "detail": reason,
            "stage": item.stage,
            "visit": item.visit,
        }),
    )
    .await
}
