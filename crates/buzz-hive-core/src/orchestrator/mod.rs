//! What the engine does to cards, one transaction per operation.
//!
//! Each operation returns the events it committed. The caller publishes them on the
//! [`crate::bus::Bus`], so a rolled-back operation wakes nobody.

pub mod cards;

use crate::domain::events::Actor;
use crate::ports::store::{CommunityId, StoreError};

/// Who is acting, and in which community.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub community: CommunityId,
    pub actor: Actor,
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    FailedPrecondition(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}
