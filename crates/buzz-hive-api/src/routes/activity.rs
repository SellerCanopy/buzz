//! A card's activity, and the full payload of one of its events.

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::orchestrator::activity::{self, ActivityFilter, MAX_ACTIVITY_LIMIT};
use buzz_hive_core::orchestrator::auto::ACTIVITY_AUTO;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AutoDecision, GetFullPayloadRequest, GetFullPayloadResponse, ListActivityRequest,
    ListActivityResponse, ListAutoDecisionsRequest, ListAutoDecisionsResponse,
};
use crate::routes::cards::to_activity;
use crate::routes::runs::parse_id;
use crate::unary::Ctx;

/// The card's activity entries, oldest first, with the newest event seq from one snapshot.
pub async fn list_activity(
    app: App,
    ctx: Ctx,
    req: ListActivityRequest,
) -> Result<ListActivityResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let author = req
        .author_kind
        .as_deref()
        .map(|kind| {
            ActorKind::parse(kind).ok_or_else(|| {
                HiveError::InvalidArgument(format!(
                    "author_kind is {kind:?}; it must be agent or person"
                ))
            })
        })
        .transpose()?;
    let visit = req
        .visit
        .map(|v| {
            u32::try_from(v)
                .map_err(|_| HiveError::InvalidArgument("visit must be 0 or more".into()))
        })
        .transpose()?;
    let filter = ActivityFilter {
        stage: req.stage_id,
        visit,
        types: req.types,
        author,
        after_seq: req.after_seq,
        limit: i64::from(req.limit),
    };
    let (events, seq) =
        activity::list_activity(app.store.as_ref(), &ctx.op(), card, &filter).await?;
    Ok(ListActivityResponse {
        entries: events
            .into_iter()
            .map(|event| {
                let by_engine = event.actor.pubkey == app.hive_pubkey;
                let mut entry = to_activity(event);
                // What the Beehive account wrote, such as an auto answer, is the engine's own.
                if let Some(author) = entry.author.as_mut().filter(|_| by_engine) {
                    author.kind = "kiln".to_owned();
                }
                entry
            })
            .collect(),
        seq,
    })
}

/// The answers auto mode gave on the card, oldest first: the card's activity entries of type
/// `auto`, each with the reason it was given.
pub async fn list_auto_decisions(
    app: App,
    ctx: Ctx,
    req: ListAutoDecisionsRequest,
) -> Result<ListAutoDecisionsResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let filter = ActivityFilter {
        types: vec![ACTIVITY_AUTO.to_owned()],
        limit: MAX_ACTIVITY_LIMIT,
        ..Default::default()
    };
    let (events, seq) =
        activity::list_activity(app.store.as_ref(), &ctx.op(), card, &filter).await?;
    let text = |event: &buzz_hive_core::domain::events::Event, key: &str| {
        event.payload[key].as_str().unwrap_or_default().to_owned()
    };
    Ok(ListAutoDecisionsResponse {
        decisions: events
            .iter()
            .map(|event| AutoDecision {
                item_id: text(event, "item"),
                kind: text(event, "kind"),
                title: text(event, "title"),
                answer: text(event, "choice"),
                reason: text(event, "detail"),
                at: Some(crate::routes::poll::timestamp(event.at)),
            })
            .collect(),
        seq,
    })
}

/// The whole payload of one event on the run's card. `full_ref` is the event's seq.
pub async fn get_full_payload(
    app: App,
    ctx: Ctx,
    req: GetFullPayloadRequest,
) -> Result<GetFullPayloadResponse, HiveError> {
    let run = parse_id(&req.run_id, "run_id")?;
    let seq: i64 = req
        .full_ref
        .parse()
        .ok()
        .filter(|seq| *seq > 0)
        .ok_or_else(|| {
            HiveError::InvalidArgument("full_ref is the event's seq, a positive number".into())
        })?;
    let payload = activity::full_payload(app.store.as_ref(), &ctx.op(), run, seq).await?;
    Ok(GetFullPayloadResponse {
        payload_json: payload.to_string(),
    })
}
