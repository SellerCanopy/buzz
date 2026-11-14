//! Posting to the Buzz relay.
//!
//! The outbox calls this port. A call may be repeated after a crash or a failed attempt,
//! so an adapter must make the repeat harmless: the same [`Delivery`] must produce the
//! same relay event, which the relay accepts once.

use chrono::{DateTime, Utc};

use crate::ports::store::Pubkey;

pub type ChannelId = uuid::Uuid;
/// A relay event id, 64 hex characters.
pub type EventId = String;

/// What makes one post the same post on every attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The outbox key, such as `kiln:{card}:{stage}:{visit}`.
    pub key: String,
    /// The time the outbox row was made. An adapter signs the event with it, so the event
    /// id does not change between attempts.
    pub at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// The relay could not be reached or did not answer. Worth trying again.
    #[error("relay unavailable: {0}")]
    Unavailable(String),
    /// The relay answered and refused the post, for example because the account is not in
    /// the channel. Trying again helps only after someone fixes the cause.
    #[error("relay refused the post: {0}")]
    Rejected(String),
}

#[async_trait::async_trait]
pub trait Relay: Send + Sync {
    /// A top-level message in `channel`.
    async fn post_status(
        &self,
        channel: ChannelId,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError>;

    /// A top-level message in `channel` that mentions `agent`. Returns the root event id.
    async fn post_stage_task(
        &self,
        channel: ChannelId,
        agent: &Pubkey,
        text: &str,
        delivery: &Delivery,
    ) -> Result<EventId, RelayError>;
}
