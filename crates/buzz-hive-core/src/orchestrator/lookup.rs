//! Remembers what the relay's directory said during one step, so the step asks once.
//!
//! The event log has one writer at a time per community: the first append of a transaction
//! takes its lock and holds it to the commit. A step that went to the relay for an agent or a
//! channel after that append kept every other writer waiting for the relay's answer. `apply`
//! asks first, through this, and the nested start of a handed-off card then finds its answers
//! here instead of asking again under the lock.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::access::RelayRole;
use crate::ports::store::{AgentInfo, AgentLink, CommunityId, Directory, PersonInfo, StoreError};

/// A [`Directory`] that keeps the answers to the lookups a step repeats.
pub(super) struct Memo<'a> {
    inner: &'a dyn Directory,
    agents: Mutex<HashMap<String, Vec<AgentInfo>>>,
    members: Mutex<HashMap<(uuid::Uuid, String), bool>>,
    channels: Mutex<HashMap<uuid::Uuid, bool>>,
}

impl<'a> Memo<'a> {
    pub(super) fn new(inner: &'a dyn Directory) -> Self {
        Self {
            inner,
            agents: Mutex::default(),
            members: Mutex::default(),
            channels: Mutex::default(),
        }
    }
}

#[async_trait::async_trait]
impl Directory for Memo<'_> {
    async fn relay_role(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<RelayRole>, StoreError> {
        self.inner.relay_role(community, pubkey).await
    }

    async fn agent_link(
        &self,
        community: CommunityId,
        pubkey: &str,
    ) -> Result<Option<AgentLink>, StoreError> {
        self.inner.agent_link(community, pubkey).await
    }

    async fn agents_named(
        &self,
        community: CommunityId,
        name: &str,
    ) -> Result<Vec<AgentInfo>, StoreError> {
        let key = name.to_lowercase();
        if let Some(found) = self.agents.lock().unwrap().get(&key) {
            return Ok(found.clone());
        }
        let found = self.inner.agents_named(community, name).await?;
        self.agents.lock().unwrap().insert(key, found.clone());
        Ok(found)
    }

    async fn list_agents(&self, community: CommunityId) -> Result<Vec<AgentInfo>, StoreError> {
        self.inner.list_agents(community).await
    }

    async fn people(&self, community: CommunityId) -> Result<Vec<PersonInfo>, StoreError> {
        self.inner.people(community).await
    }

    async fn channel_exists(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
    ) -> Result<bool, StoreError> {
        if let Some(exists) = self.channels.lock().unwrap().get(&channel) {
            return Ok(*exists);
        }
        let exists = self.inner.channel_exists(community, channel).await?;
        self.channels.lock().unwrap().insert(channel, exists);
        Ok(exists)
    }

    async fn member_of_channel(
        &self,
        community: CommunityId,
        channel: uuid::Uuid,
        pubkey: &str,
    ) -> Result<bool, StoreError> {
        let key = (channel, pubkey.to_owned());
        if let Some(member) = self.members.lock().unwrap().get(&key) {
            return Ok(*member);
        }
        let member = self
            .inner
            .member_of_channel(community, channel, pubkey)
            .await?;
        self.members.lock().unwrap().insert(key, member);
        Ok(member)
    }
}
