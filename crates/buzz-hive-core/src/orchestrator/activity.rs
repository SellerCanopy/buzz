//! What the agent of a running stage leaves in the card's activity: the work it finished and
//! the choices it made. The next session on the stage reads them back as a brief.
//!
//! Ported from the Go engine's activity tools (`adapters/mcptools/activity.go`). Beehive has no
//! blocker tool, so an entry is progress, a decision, or a person's note.

use serde_json::json;
use uuid::Uuid;

use super::run::{check_agent, locked, running_stage};
use super::{Ctx, Env, OpError};
use crate::domain::events::Event;
use crate::ports::store::Tx;
use crate::rules::brief::{brief as lay_out, Entry, EntryKind, BRIEF_CAP};

/// The longest summary of an entry, in characters.
pub const MAX_SUMMARY_CHARS: usize = 120;
/// The longest detail of an entry, in characters.
pub const MAX_DETAIL_CHARS: usize = 2000;
/// The longest reason a decision gives, in characters.
pub const MAX_WHY_CHARS: usize = 300;
/// The longest option a decision names as turned down, in characters.
pub const MAX_OPTION_CHARS: usize = 120;
/// The longest reason for turning an option down, in characters.
pub const MAX_REJECTED_REASON_CHARS: usize = 300;
/// The most options a decision may name as turned down.
pub const MAX_REJECTED: usize = 10;

/// `read_history` returns this many entries when the caller names no limit.
pub const DEFAULT_HISTORY_LIMIT: i64 = 100;
/// The most entries `read_history` returns.
pub const MAX_HISTORY_LIMIT: i64 = 500;

const KIND: &str = "activity.added";

/// An option a decision turned down, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub option: String,
    pub reason: String,
}

fn check_text(what: &str, text: &str, max: usize) -> Result<(), OpError> {
    if text.chars().count() > max {
        return Err(OpError::InvalidArgument(format!(
            "{what} must be at most {max} characters"
        )));
    }
    Ok(())
}

fn check_summary(summary: &str) -> Result<String, OpError> {
    let summary = summary.trim();
    if summary.is_empty() || summary.contains('\n') || summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "summary must be one line of 1 to {MAX_SUMMARY_CHARS} characters"
        )));
    }
    Ok(summary.to_owned())
}

/// Appends an entry for the card's running stage. Only that stage's agent may.
async fn log(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    mut payload: serde_json::Value,
) -> Result<Event, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (_, run, _) = locked(env, &mut *tx, card_id).await?;
    let stage = running_stage(&run)?;
    check_agent(&mut *tx, ctx, &run, &stage).await?;
    payload["stage"] = json!(stage);
    payload["visit"] = json!(run.visit);
    super::routes::append(&mut *tx, ctx, KIND, &run, payload).await?;
    let mut events = tx.commit().await?;
    events
        .pop()
        .ok_or_else(|| OpError::FailedPrecondition("the entry was not saved".into()))
}

/// The stage's agent logs one piece of finished work. The entry's seq is the number it is
/// known by.
pub async fn log_work(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    summary: &str,
    detail: &str,
    plan_item: &str,
) -> Result<Event, OpError> {
    let summary = check_summary(summary)?;
    check_text("detail", detail, MAX_DETAIL_CHARS)?;
    check_text(
        "plan_item",
        plan_item,
        super::artifacts::MAX_PLAN_ITEM_BYTES,
    )?;
    log(
        env,
        ctx,
        card_id,
        json!({ "type": "progress", "summary": summary, "detail": detail, "plan_item": plan_item }),
    )
    .await
}

/// The stage's agent records a choice and why. `supersedes` is the entry number of an earlier
/// decision this one replaces, 0 for none.
#[allow(clippy::too_many_arguments)]
pub async fn record_decision(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    summary: &str,
    why: &str,
    rejected: &[Rejected],
    detail: &str,
    supersedes: i64,
) -> Result<Event, OpError> {
    let summary = check_summary(summary)?;
    let why = why.trim();
    if why.is_empty() {
        return Err(OpError::InvalidArgument("why is required".into()));
    }
    check_text("why", why, MAX_WHY_CHARS)?;
    check_text("detail", detail, MAX_DETAIL_CHARS)?;
    if rejected.len() > MAX_REJECTED {
        return Err(OpError::InvalidArgument(format!(
            "rejected may name at most {MAX_REJECTED} options"
        )));
    }
    for r in rejected {
        check_text("a rejected option", &r.option, MAX_OPTION_CHARS)?;
        check_text(
            "the reason for a rejected option",
            &r.reason,
            MAX_REJECTED_REASON_CHARS,
        )?;
    }
    if supersedes < 0 {
        return Err(OpError::InvalidArgument(
            "supersedes must not be negative".into(),
        ));
    }
    let rejected: Vec<_> = rejected
        .iter()
        .map(|r| json!({ "option": r.option, "reason": r.reason }))
        .collect();
    log(
        env,
        ctx,
        card_id,
        json!({
            "type": "decision",
            "summary": summary,
            "detail": detail,
            "why": why,
            "rejected": rejected,
            "supersedes": supersedes,
        }),
    )
    .await
}

/// The card's activity entries after `after_seq`, oldest first. `limit` 0 means the default.
pub async fn read_history(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    after_seq: i64,
    limit: i64,
) -> Result<Vec<Event>, OpError> {
    if !(0..=MAX_HISTORY_LIMIT).contains(&limit) {
        return Err(OpError::InvalidArgument(format!(
            "limit is {limit}; it must be 1 to {MAX_HISTORY_LIMIT}, or 0 for {DEFAULT_HISTORY_LIMIT}"
        )));
    }
    let limit = if limit == 0 {
        DEFAULT_HISTORY_LIMIT
    } else {
        limit
    };
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    Ok(tx
        .events()
        .of_card(card_id, KIND, after_seq.max(0), limit)
        .await?)
}

/// `list_activity` returns this many entries when the caller names no limit.
pub const DEFAULT_ACTIVITY_LIMIT: i64 = 500;
/// The most entries `list_activity` returns.
pub const MAX_ACTIVITY_LIMIT: i64 = 1000;

/// Which of a card's activity entries `list_activity` keeps. A field left empty keeps all.
#[derive(Debug, Clone, Default)]
pub struct ActivityFilter {
    pub stage: Option<String>,
    pub visit: Option<u32>,
    /// progress, decision, answer, note or message.
    pub types: Vec<String>,
    pub author: Option<crate::access::ActorKind>,
    pub after_seq: i64,
    /// 0 means [`DEFAULT_ACTIVITY_LIMIT`].
    pub limit: i64,
}

/// The card's activity entries that pass `filter`, oldest first, and the newest event seq,
/// read in one transaction so a page can watch from it.
pub async fn list_activity(
    store: &dyn crate::ports::store::Store,
    ctx: &Ctx,
    card_id: Uuid,
    filter: &ActivityFilter,
) -> Result<(Vec<Event>, i64), OpError> {
    if !(0..=MAX_ACTIVITY_LIMIT).contains(&filter.limit) {
        return Err(OpError::InvalidArgument(format!(
            "limit is {}; it must be 1 to {MAX_ACTIVITY_LIMIT}, or 0 for {DEFAULT_ACTIVITY_LIMIT}",
            filter.limit
        )));
    }
    let limit = match filter.limit {
        0 => DEFAULT_ACTIVITY_LIMIT,
        n => n,
    };
    let mut tx = store.begin(ctx.community).await?;
    let seq = tx.events().latest_seq().await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let keeps = |event: &Event| {
        let p = &event.payload;
        filter
            .stage
            .as_deref()
            .is_none_or(|stage| p["stage"].as_str() == Some(stage))
            && filter
                .visit
                .is_none_or(|visit| p["visit"].as_u64() == Some(u64::from(visit)))
            && (filter.types.is_empty()
                || p["type"]
                    .as_str()
                    .is_some_and(|t| filter.types.iter().any(|want| want == t)))
            && filter.author.is_none_or(|kind| event.actor.kind == kind)
    };
    // The filter runs here, so a page of the log may hold none that pass. Keep reading
    // until the limit is met or the log ends.
    let mut kept: Vec<Event> = Vec::new();
    let mut after = filter.after_seq.max(0);
    while (kept.len() as i64) < limit {
        let page = tx
            .events()
            .of_card(card_id, KIND, after, MAX_ACTIVITY_LIMIT)
            .await?;
        let Some(last) = page.last().map(|event| event.seq) else {
            break;
        };
        let more = page.len() as i64 == MAX_ACTIVITY_LIMIT;
        kept.extend(page.into_iter().filter(|event| keeps(event)));
        after = last;
        if !more {
            break;
        }
    }
    kept.truncate(limit as usize);
    Ok((kept, seq))
}

/// The payload of the event `seq`, when it belongs to the run's card. Events are never cut,
/// so this is the whole of what the activity entry summarizes.
pub async fn full_payload(
    store: &dyn crate::ports::store::Store,
    ctx: &Ctx,
    run_id: Uuid,
    seq: i64,
) -> Result<serde_json::Value, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let run = tx
        .runs()
        .get(run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {run_id} does not exist")))?;
    let event = tx
        .events()
        .after(seq.saturating_sub(1), 1)
        .await?
        .into_iter()
        .next()
        .filter(|event| event.seq == seq && event.card_id == Some(run.card_id))
        .ok_or_else(|| OpError::NotFound(format!("event {seq} is not on run {run_id}'s card")))?;
    Ok(event.payload)
}

/// One entry as the brief reads it, or `None` for an entry of another stage or another kind.
fn entry_of(event: &Event, stage: &str) -> Option<Entry> {
    let p = &event.payload;
    let text = |key: &str| p.get(key).and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let kind = match p.get("type")?.as_str()? {
        "progress" if p.get("stage")?.as_str()? == stage => EntryKind::Progress,
        "decision" if p.get("stage")?.as_str()? == stage => EntryKind::Decision {
            why: text("why"),
            rejected: p
                .get("rejected")
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .map(|r| {
                            let get = |k: &str| {
                                r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_owned()
                            };
                            (get("option"), get("reason"))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            supersedes: p.get("supersedes").and_then(|v| v.as_i64()).unwrap_or(0),
        },
        "note" => EntryKind::Note,
        _ => return None,
    };
    Some(Entry {
        seq: event.seq,
        kind,
        summary: text("summary"),
        detail: text("detail"),
    })
}

/// What earlier sessions on `stage` left for the next one, laid out within [`BRIEF_CAP`]
/// characters. Empty when there is nothing.
pub(super) async fn brief_for(tx: &mut dyn Tx, card: Uuid, stage: &str) -> Result<String, OpError> {
    let mut entries = Vec::new();
    let mut after = 0;
    loop {
        let page = tx.events().of_card(card, KIND, after, 500).await?;
        let done = page.len() < 500;
        if let Some(last) = page.last() {
            after = last.seq;
        }
        entries.extend(page.iter().filter_map(|e| entry_of(e, stage)));
        if done {
            return Ok(lay_out(&entries, BRIEF_CAP));
        }
    }
}

/// The brief for `stage`, or for the stage the card's run is at when none is named.
pub async fn get_brief(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    stage: &str,
) -> Result<String, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let stage = if stage.is_empty() {
        match tx
            .runs()
            .active_for_card(card_id)
            .await?
            .and_then(|r| r.current_stage)
        {
            Some(stage) => stage,
            None => return Ok(String::new()),
        }
    } else {
        stage.to_owned()
    };
    brief_for(&mut *tx, card_id, &stage).await
}
