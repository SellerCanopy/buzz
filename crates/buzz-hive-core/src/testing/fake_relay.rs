use std::sync::{Arc, Mutex};

use crate::ports::relay::{ChannelId, Delivery, EventId, Relay, RelayError};
use crate::ports::store::Pubkey;

/// What a post looked like when it reached the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub key: String,
    pub channel: ChannelId,
    /// The agent the post mentions, `None` for a status line.
    pub mention: Option<Pubkey>,
    pub text: String,
    pub event_id: EventId,
}

#[derive(Default)]
struct Inner {
    /// One entry per key, like a relay that accepts an event id once.
    posts: Vec<Posted>,
    /// Attempts that fail before posts start to succeed.
    failures_left: u32,
    /// Every call made, including the ones that failed and the repeats.
    calls: Vec<String>,
    refuse_with: Option<String>,
}

/// A [`Relay`] that records posts by key. A key posted twice records one post and returns
/// the same event id both times, as a relay does for an event it already has.
#[derive(Clone, Default)]
pub struct FakeRelay {
    inner: Arc<Mutex<Inner>>,
}

impl FakeRelay {
    pub fn new() -> Self {
        Self::default()
    }

    /// The first `n` calls fail as if the relay were down.
    pub fn failing_first(self, n: u32) -> Self {
        self.inner.lock().unwrap().failures_left = n;
        self
    }

    /// Every call is refused with `reason`.
    pub fn refusing(self, reason: &str) -> Self {
        self.inner.lock().unwrap().refuse_with = Some(reason.to_owned());
        self
    }

    pub fn posts(&self) -> Vec<Posted> {
        self.inner.lock().unwrap().posts.clone()
    }

    /// The keys of every call in order, repeats and failures included.
    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    fn post(
        &self,
        channel: ChannelId,
        mention: Option<&Pubkey>,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(delivery.key.clone());
        if let Some(reason) = &inner.refuse_with {
            return Err(RelayError::Rejected(reason.clone()));
        }
        if inner.failures_left > 0 {
            inner.failures_left -= 1;
            return Err(RelayError::Unavailable("the relay is down".into()));
        }
        if let Some(known) = inner.posts.iter().find(|p| p.key == delivery.key) {
            return Ok(known.event_id.clone());
        }
        // The id depends only on what the adapter signs, as a real event id does.
        let event_id = event_id(channel, mention, text, delivery);
        inner.posts.push(Posted {
            key: delivery.key.clone(),
            channel,
            mention: mention.cloned(),
            text: text.to_owned(),
            event_id: event_id.clone(),
        });
        Ok(event_id)
    }
}

fn event_id(
    channel: ChannelId,
    mention: Option<&Pubkey>,
    text: &str,
    delivery: &Delivery,
) -> EventId {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(channel.as_bytes());
    hash.update(mention.map_or("", |m| m.as_str()));
    hash.update(text);
    hash.update(delivery.at.timestamp().to_le_bytes());
    hex::encode(hash.finalize())
}

#[async_trait::async_trait]
impl Relay for FakeRelay {
    async fn post_status(
        &self,
        channel: ChannelId,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.post(channel, None, text, delivery)
    }

    async fn post_stage_task(
        &self,
        channel: ChannelId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError> {
        self.post(channel, Some(agent), text, delivery)
    }
}
