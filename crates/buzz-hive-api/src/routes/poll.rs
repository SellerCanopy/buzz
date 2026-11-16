//! `PollEvents`: the one long-poll call every live screen uses.

use std::time::Duration;

use buzz_hive_core::domain::events::Event;
use tokio::time::Instant;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{Event as ProtoEvent, PollEventsRequest, PollEventsResponse};
use crate::unary::Ctx;

/// Longest a call waits for an event. A longer request is clamped to it.
pub const MAX_WAIT: Duration = Duration::from_secs(25);
/// Most events one call returns.
pub const PAGE: i64 = 500;

pub async fn poll_events(
    app: App,
    ctx: Ctx,
    req: PollEventsRequest,
) -> Result<PollEventsResponse, HiveError> {
    if req.after_seq < 0 {
        return Err(HiveError::InvalidArgument(
            "after_seq must not be negative".into(),
        ));
    }
    let wait = Duration::from_secs(req.wait_seconds.max(0) as u64).min(MAX_WAIT);
    let deadline = Instant::now() + wait;
    // Subscribe before the first read: a commit between the read and the wait still
    // wakes `changed()`, because the receiver has not seen that value yet.
    let mut rx = app.bus.subscribe();
    loop {
        let events = app
            .store
            .events_after(ctx.community, req.after_seq, PAGE)
            .await?;
        if let Some(last) = events.last() {
            let next_seq = last.seq;
            let viewer = ctx.op().requester_or_actor();
            return Ok(PollEventsResponse {
                events: events
                    .into_iter()
                    .map(|event| to_proto(for_viewer(event, &viewer)))
                    .collect(),
                next_seq,
            });
        }
        let woke = tokio::select! {
            changed = tokio::time::timeout_at(deadline, rx.changed()) => matches!(changed, Ok(Ok(()))),
            () = app.bus.closed() => false,
        };
        if !woke {
            return Ok(PollEventsResponse {
                events: Vec::new(),
                next_seq: req.after_seq,
            });
        }
    }
}

/// The assistant's reply is for the person who asked, and the log is read by everyone who may
/// read the board, so `job.result` keeps its `message` only for the job's requester.
fn for_viewer(mut event: Event, viewer: &str) -> Event {
    if event.kind == "job.result" && event.payload["requester"].as_str() != Some(viewer) {
        if let Some(payload) = event.payload.as_object_mut() {
            payload.remove("message");
        }
    }
    event
}

pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> pbjson_types::Timestamp {
    pbjson_types::Timestamp {
        seconds: at.timestamp(),
        nanos: at.timestamp_subsec_nanos() as i32,
    }
}

pub(super) fn to_proto(event: Event) -> ProtoEvent {
    ProtoEvent {
        seq: event.seq,
        r#type: event.kind,
        run_id: event.run_id.map(|id| id.to_string()).unwrap_or_default(),
        card_id: event.card_id.map(|id| id.to_string()).unwrap_or_default(),
        payload_json: event.payload.to_string(),
        at: Some(timestamp(event.at)),
        actor_pubkey: event.actor.pubkey,
        actor_kind: event.actor.kind.as_str().to_owned(),
    }
}
