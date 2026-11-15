//! Posts to the Buzz relay as the Beehive account.
//!
//! The outbox may send one post more than once: after a failed attempt, or after a crash
//! between the send and the `done` mark. Every attempt of a post builds the same event,
//! signed with the time the outbox row was made, so the relay sees one event id and keeps
//! one message.

use buzz_hive_core::ports::relay::{ChannelId, Delivery, EventId, Relay, RelayError};
use buzz_hive_core::ports::store::Pubkey;
use buzz_ws_client::{HeaderMap, OkResponse, WsClientError};
use nostr::{Event, Keys, Timestamp};

/// The relay's limit for a message, in bytes.
pub const MAX_CONTENT_BYTES: usize = buzz_hive_core::orchestrator::task_message::MAX_POST_BYTES;

/// Added in place of the text cut off, so a reader sees that the message is not whole.
pub const TRUNCATED: &str = "\n\n(truncated, see the card)";

/// Seconds one post may take, connecting and signing in included.
const POST_TIMEOUT_SECS: u64 = 60;

pub struct RelayAdapter {
    keys: Keys,
    relay_url: String,
}

impl RelayAdapter {
    pub fn new(keys: Keys, relay_url: String) -> Self {
        Self { keys, relay_url }
    }

    /// The account's pubkey as 64 hex characters.
    pub fn pubkey(&self) -> Pubkey {
        self.keys.public_key().to_hex()
    }

    /// The event for one post. The same arguments always give the same event.
    pub fn build(
        &self,
        channel: ChannelId,
        text: &str,
        mentions: &[&str],
        delivery: &Delivery,
    ) -> Result<Event, RelayError> {
        self.sign(channel, text, None, mentions, delivery)
    }

    /// The event for a reply in the thread `root` opened. The reply's parent is the root, so
    /// the thread stays flat.
    pub fn build_reply(
        &self,
        channel: ChannelId,
        root: &EventId,
        text: &str,
        mentions: &[&str],
        delivery: &Delivery,
    ) -> Result<Event, RelayError> {
        let root = nostr::EventId::from_hex(root).map_err(|e| {
            RelayError::Rejected(format!("thread root {root} is not an event id: {e}"))
        })?;
        let thread = buzz_sdk::ThreadRef {
            root_event_id: root,
            parent_event_id: root,
        };
        self.sign(channel, text, Some(&thread), mentions, delivery)
    }

    fn sign(
        &self,
        channel: ChannelId,
        text: &str,
        thread: Option<&buzz_sdk::ThreadRef>,
        mentions: &[&str],
        delivery: &Delivery,
    ) -> Result<Event, RelayError> {
        let text = fit(text);
        let at = u64::try_from(delivery.at.timestamp()).unwrap_or_default();
        buzz_sdk::builders::build_message(channel, &text, thread, mentions, false, &[], &[])
            .map_err(|e| RelayError::Rejected(e.to_string()))?
            .custom_created_at(Timestamp::from_secs(at))
            .sign_with_keys(&self.keys)
            .map_err(|e| RelayError::Rejected(e.to_string()))
    }

    async fn publish(&self, event: Event) -> Result<EventId, RelayError> {
        let id = event.id.to_hex();
        let ok = buzz_ws_client::publish_event(
            &self.relay_url,
            event,
            &self.keys,
            None,
            HeaderMap::new(),
            POST_TIMEOUT_SECS,
        )
        .await
        .map_err(failure)?;
        judge(id, ok)
    }
}

#[async_trait::async_trait]
impl Relay for RelayAdapter {
    async fn post_status(
        &self,
        channel: ChannelId,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.publish(self.build(channel, text, &[], delivery)?)
            .await
    }

    async fn post_stage_task(
        &self,
        channel: ChannelId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.publish(self.build(channel, text, &[agent.as_str()], delivery)?)
            .await
    }

    async fn post_reply(
        &self,
        channel: ChannelId,
        root: &EventId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.publish(self.build_reply(channel, root, text, &[agent.as_str()], delivery)?)
            .await
    }
}

/// `text` as it will be sent: whole when it fits, else cut at a character boundary with
/// [`TRUNCATED`] at the end, so the message is never silently short.
pub fn fit(text: &str) -> String {
    if text.len() <= MAX_CONTENT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_CONTENT_BYTES - TRUNCATED.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &text[..end])
}

/// What the relay's answer to a post means. A repeat of an event the relay already holds
/// (`duplicate: already have it`) is the outcome a retry wants, so the post is done. Any other
/// refusal is final.
fn judge(id: EventId, ok: OkResponse) -> Result<EventId, RelayError> {
    if ok.accepted || ok.message.starts_with("duplicate") {
        Ok(id)
    } else {
        Err(RelayError::Rejected(ok.message))
    }
}

/// Whether a failed post is worth trying again unchanged. The relay refusing the event or
/// the account is final: the same post would be refused again. A connection that did not
/// complete, or an answer that was not understood, may be gone by the next attempt. Each
/// variant is named, so a new one has to be decided here.
fn failure(error: WsClientError) -> RelayError {
    match error {
        WsClientError::EventRejected(_)
        | WsClientError::AuthFailed(_)
        | WsClientError::PolicyClose(_) => RelayError::Rejected(error.to_string()),
        WsClientError::WebSocket(_)
        | WsClientError::Json(_)
        | WsClientError::EventBuilder(_)
        | WsClientError::Url(_)
        | WsClientError::Timeout
        | WsClientError::ConnectionClosed
        | WsClientError::UnexpectedMessage(_)
        | WsClientError::NoAuthChallenge => RelayError::Unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn adapter() -> RelayAdapter {
        RelayAdapter::new(Keys::generate(), "ws://localhost:3000".into())
    }

    fn delivery(secs: i64) -> Delivery {
        Delivery {
            key: "kiln:c:s:1".into(),
            at: Utc.timestamp_opt(secs, 123_456_789).unwrap(),
        }
    }

    fn channel() -> ChannelId {
        uuid::Uuid::from_u128(77)
    }

    #[test]
    fn a_retry_builds_the_same_event() {
        let adapter = adapter();
        let agent = Keys::generate().public_key().to_hex();
        let first = adapter
            .build(channel(), "hello", &[&agent], &delivery(1_700_000_000))
            .unwrap();
        let again = adapter
            .build(channel(), "hello", &[&agent], &delivery(1_700_000_000))
            .unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(first.created_at.as_secs(), 1_700_000_000);
    }

    #[test]
    fn the_event_is_a_channel_message_that_mentions_the_agent() {
        let adapter = adapter();
        let agent = Keys::generate().public_key().to_hex();
        let event = adapter
            .build(
                channel(),
                "do the stage",
                &[&agent],
                &delivery(1_700_000_000),
            )
            .unwrap();
        assert_eq!(event.kind.as_u16(), 9);
        assert_eq!(event.pubkey.to_hex(), adapter.pubkey());
        let tags: Vec<Vec<String>> = event.tags.iter().map(|t| t.clone().to_vec()).collect();
        assert!(tags.contains(&vec!["h".into(), channel().to_string()]));
        assert!(tags.contains(&vec!["p".into(), agent]));
        assert!(event.verify().is_ok());
    }

    #[test]
    fn a_reply_points_at_its_root_and_mentions_the_agent() {
        let adapter = adapter();
        let agent = Keys::generate().public_key().to_hex();
        let root = adapter
            .build(channel(), "stage task", &[&agent], &delivery(1_700_000_000))
            .unwrap();
        let reply = adapter
            .build_reply(
                channel(),
                &root.id.to_hex(),
                "round 2",
                &[&agent],
                &delivery(1_700_000_001),
            )
            .unwrap();
        let tags: Vec<Vec<String>> = reply.tags.iter().map(|t| t.clone().to_vec()).collect();
        let root_id = root.id.to_hex();
        assert!(
            tags.iter()
                .any(|t| t[0] == "e" && t[1] == root_id && t.get(3).is_some_and(|m| m == "reply")),
            "{tags:?}"
        );
        assert!(tags.contains(&vec!["p".into(), agent.clone()]));
        assert!(reply.verify().is_ok());
        let again = adapter
            .build_reply(
                channel(),
                &root.id.to_hex(),
                "round 2",
                &[&agent],
                &delivery(1_700_000_001),
            )
            .unwrap();
        assert_eq!(reply.id, again.id, "a retry builds the same reply");
    }

    #[test]
    fn a_reply_to_a_root_that_is_not_an_event_id_is_refused() {
        let error = adapter()
            .build_reply(channel(), &"nope".to_owned(), "x", &[], &delivery(1))
            .unwrap_err();
        assert!(matches!(error, RelayError::Rejected(_)), "{error}");
    }

    #[test]
    fn a_status_mentions_no_one() {
        let event = adapter()
            .build(channel(), "run started", &[], &delivery(1_700_000_000))
            .unwrap();
        assert!(event.tags.iter().all(|t| t.clone().to_vec()[0] != "p"));
    }

    #[test]
    fn another_key_or_text_is_another_event() {
        let adapter = adapter();
        let a = adapter
            .build(channel(), "one", &[], &delivery(1_700_000_000))
            .unwrap();
        let b = adapter
            .build(channel(), "two", &[], &delivery(1_700_000_000))
            .unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn a_bad_mention_is_refused_not_retried() {
        let error = adapter()
            .build(channel(), "x", &["not hex"], &delivery(1_700_000_000))
            .unwrap_err();
        assert!(matches!(error, RelayError::Rejected(_)), "{error}");
    }

    #[test]
    fn short_text_is_sent_whole() {
        assert_eq!(fit("hello"), "hello");
        let exact = "a".repeat(MAX_CONTENT_BYTES);
        assert_eq!(fit(&exact), exact);
    }

    #[test]
    fn long_text_is_cut_with_a_visible_suffix_that_fits() {
        let long = "a".repeat(MAX_CONTENT_BYTES + 10);
        let cut = fit(&long);
        assert_eq!(cut.len(), MAX_CONTENT_BYTES);
        assert!(cut.ends_with("(truncated, see the card)"));
        // A cut inside a multi-byte character moves back to the boundary.
        let wide = "é".repeat(MAX_CONTENT_BYTES);
        let cut = fit(&wide);
        assert!(cut.len() <= MAX_CONTENT_BYTES);
        assert!(cut.ends_with("(truncated, see the card)"));
    }

    fn answer(accepted: bool, message: &str) -> OkResponse {
        OkResponse {
            event_id: "ab".repeat(32),
            accepted,
            message: message.into(),
        }
    }

    #[test]
    fn an_accepted_post_and_a_duplicate_of_one_are_both_done() {
        let id = "ab".repeat(32);
        assert_eq!(judge(id.clone(), answer(true, "")).unwrap(), id);
        // The wording the relay uses for an event it already holds, which a replay after a
        // crash between the send and the done mark meets.
        let again = answer(false, "duplicate: already have it");
        assert_eq!(judge(id.clone(), again).unwrap(), id);
    }

    #[test]
    fn any_other_refusal_is_final() {
        for message in [
            "blocked: not a member",
            "invalid: bad signature",
            "rate-limited: slow down",
            "",
            "not a duplicate: duplicate",
        ] {
            match judge("ab".repeat(32), answer(false, message)) {
                Err(RelayError::Rejected(got)) => assert_eq!(got, message),
                other => panic!("{message:?}: want a final refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_refusal_by_the_relay_is_final_and_a_failed_connection_is_tried_again() {
        let rejected = [
            WsClientError::EventRejected("blocked".into()),
            WsClientError::AuthFailed("no".into()),
            WsClientError::PolicyClose("policy".into()),
        ];
        for error in rejected {
            let text = error.to_string();
            match failure(error) {
                RelayError::Rejected(got) => assert_eq!(got, text),
                other => panic!("{text}: want a final refusal, got {other:?}"),
            }
        }
        let unavailable = [
            WsClientError::Json(serde_json::from_str::<u8>("x").unwrap_err()),
            WsClientError::EventBuilder("x".into()),
            WsClientError::Url("x".into()),
            WsClientError::Timeout,
            WsClientError::ConnectionClosed,
            WsClientError::UnexpectedMessage("x".into()),
            WsClientError::NoAuthChallenge,
        ];
        for error in unavailable {
            let text = error.to_string();
            match failure(error) {
                RelayError::Unavailable(got) => assert_eq!(got, text),
                other => panic!("{text}: want a retry, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_relay_that_cannot_be_reached_leaves_the_post_to_be_tried_again() {
        // A port nothing listens on: the listener is closed before the post is sent.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let adapter = RelayAdapter::new(Keys::generate(), format!("ws://127.0.0.1:{port}"));
        let event = adapter
            .build(channel(), "hello", &[], &delivery(1_700_000_000))
            .unwrap();
        let error = adapter.publish(event).await.unwrap_err();
        assert!(matches!(error, RelayError::Unavailable(_)), "{error}");
    }

    #[test]
    fn the_message_that_is_sent_passes_the_relay_size_check() {
        let event = adapter()
            .build(
                channel(),
                &"b".repeat(MAX_CONTENT_BYTES * 2),
                &[],
                &delivery(1_700_000_000),
            )
            .unwrap();
        assert!(event.content.ends_with("(truncated, see the card)"));
    }
}
