//! Storage port: one transaction per community, plus read-only relay facts.

use crate::access::{ActorKind, Level, RelayRole};

pub type CommunityId = uuid::Uuid;
/// 64 hex characters, lowercase.
pub type Pubkey = String;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store unavailable: {0}")]
    Unavailable(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
}

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError>;
}

/// One transaction. Dropping it without `commit` rolls it back.
///
/// `commit` returns `()` in this phase and `Vec<Event>` from Phase 4.
#[async_trait::async_trait]
pub trait Tx: Send {
    fn access(&mut self) -> &mut dyn AccessRepo;
    fn settings(&mut self) -> &mut dyn SettingsRepo;
    async fn commit(self: Box<Self>) -> Result<(), StoreError>;
}

#[async_trait::async_trait]
pub trait AccessRepo: Send {
    async fn level(&mut self, pubkey: &str) -> Result<Option<(Level, ActorKind)>, StoreError>;
    async fn set_level(
        &mut self,
        pubkey: &str,
        level: Level,
        kind: ActorKind,
        by: &str,
    ) -> Result<(), StoreError>;
    async fn list(&mut self) -> Result<Vec<AccessRow>, StoreError>;
}

#[async_trait::async_trait]
pub trait SettingsRepo: Send {
    async fn get(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError>;
    async fn set(
        &mut self,
        scope: &str,
        scope_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), StoreError>;
}

/// Relay facts, read-only. Implemented by `buzz-hive-store::relay_read`.
#[async_trait::async_trait]
pub trait Directory: Send + Sync {
    async fn relay_role(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError>;
    /// `Some(owner pubkey)` when the pubkey is an agent, `None` when it is a person or unknown.
    async fn agent_owner(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<Pubkey>, StoreError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccessRow {
    pub pubkey: Pubkey,
    pub level: Level,
    pub kind: ActorKind,
}
