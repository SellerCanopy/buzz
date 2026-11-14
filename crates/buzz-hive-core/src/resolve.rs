//! Turn a pubkey into the level it acts at.

use crate::access::{cap_agent, starting_level, ActorKind, Level};
use crate::ports::store::{CommunityId, Directory, Pubkey, StoreError, Tx};

/// Longest agent-to-owner chain followed. Owners are people in practice; the
/// bound keeps a corrupt or hostile relay row from looping.
pub const MAX_OWNER_CHAIN: usize = 4;

pub struct Actor {
    pub pubkey: Pubkey,
    pub kind: ActorKind,
    pub level: Level,
    pub owner: Option<Pubkey>,
}

/// Resolve a pubkey's effective starting level from the store and the relay.
///
/// A stored level wins over the relay-derived one, and an agent's stored level
/// is capped at its owner's resolved level. An owner chain longer than
/// [`MAX_OWNER_CHAIN`], or one that loops, is a `Conflict`.
pub async fn resolve_actor(
    tx: &mut dyn Tx,
    dir: &dyn Directory,
    community: CommunityId,
    pubkey: &str,
) -> Result<Actor, StoreError> {
    // Walk from the pubkey up through its owners.
    let mut chain: Vec<(Pubkey, Option<Pubkey>)> = Vec::new();
    let mut current = pubkey.to_owned();
    loop {
        let owner = dir.agent_owner(community, &current).await?;
        if chain.iter().any(|(seen, _)| *seen == current) {
            return Err(StoreError::Conflict("agent owner chain loops".into()));
        }
        chain.push((current.clone(), owner.clone()));
        match owner {
            Some(next) => {
                if chain.len() > MAX_OWNER_CHAIN {
                    return Err(StoreError::Conflict("agent owner chain too long".into()));
                }
                current = next;
            }
            None => break,
        }
    }

    // Resolve from the top of the chain down, so each agent sees its owner's level.
    let mut owner_level: Option<Level> = None;
    let mut resolved: Option<Actor> = None;
    for (key, owner) in chain.into_iter().rev() {
        let kind = if owner.is_some() {
            ActorKind::Agent
        } else {
            ActorKind::Person
        };
        let stored = tx.access().level(&key).await?.map(|(level, _)| level);
        let level = match stored {
            Some(level) => owner_level.map_or(level, |ol| cap_agent(level, ol)),
            None => starting_level(kind, dir.relay_role(community, &key).await?),
        };
        owner_level = Some(level);
        resolved = Some(Actor {
            pubkey: key,
            kind,
            level,
            owner,
        });
    }
    Ok(resolved.expect("the chain always holds the starting pubkey"))
}
