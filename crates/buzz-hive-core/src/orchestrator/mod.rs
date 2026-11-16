//! What the engine does to cards, one transaction per operation.
//!
//! Each operation returns the events it committed. The caller publishes them on the
//! [`crate::bus::Bus`], so a rolled-back operation wakes nobody.

pub mod access;
pub mod activity;
pub mod artifacts;
pub mod auto;
pub mod cards;
pub mod inbox;
pub mod intake;
mod lookup;
pub mod merge;
pub mod outbox;
pub mod pull_request;
mod routes;
pub mod run;
pub mod settings;
pub mod task_message;
pub mod timers;
pub mod usage;
pub mod wait;
pub mod workflows;

use crate::domain::events::Actor;
use crate::ports::store::{CommunityId, Directory, Pubkey, Store, StoreError};
use crate::workflow::load::Catalog;

/// Who is acting, and in which community.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub community: CommunityId,
    pub actor: Actor,
    /// The person an agent acts for, from its requester claim. A run the agent starts keeps
    /// this pubkey as its requester, so the agents that work it act at that person's level.
    /// `None` for a person, and for an agent with no claim.
    pub requester: Option<Pubkey>,
}

/// What the operations that reach past the store need.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    pub store: &'a dyn Store,
    pub directory: &'a dyn Directory,
    /// The packages as they are on disk right now.
    pub catalog: &'a Catalog,
    /// The Beehive account's pubkey, the one that posts and must be in the update channel.
    pub hive: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    FailedPrecondition(String),
    /// The caller is known but may not do this, whatever its access level.
    #[error("{0}")]
    PermissionDenied(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}
