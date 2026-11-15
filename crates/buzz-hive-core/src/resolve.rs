//! Turn a pubkey into the level it acts at.

use crate::access::{cap_agent, starting_level, ActorKind, Level, RelayRole};
use crate::ports::store::{CommunityId, Directory, Pubkey, Store, StoreError};

/// Longest agent-to-owner chain followed. Owners are people in practice; the
/// bound keeps a corrupt or hostile relay row from looping.
pub const MAX_OWNER_CHAIN: usize = 4;

pub struct Actor {
    pub pubkey: Pubkey,
    pub kind: ActorKind,
    pub level: Level,
    pub owner: Option<Pubkey>,
    /// Whether the relay still counts this pubkey as part of the community. A person is a
    /// member while the relay lists a role for them. An agent is a member while it is not
    /// deactivated and its owner is a member. Only a member may get a token, and a
    /// non-member's stored level counts for nothing: `level` is `None`.
    pub member: bool,
}

/// One step of the walk from a pubkey up through its owners, as the relay reports it.
struct Link {
    key: Pubkey,
    /// The agent's owner, `None` for a person.
    owner: Option<Pubkey>,
    deactivated: bool,
    /// A person's role in the relay's member list. Not read for an agent.
    role: Option<RelayRole>,
}

/// Resolve a pubkey's effective level from the relay and the store.
///
/// The relay is asked first, with no store transaction open, and the stored levels are read
/// afterwards in one short transaction. So a request never holds a store connection while it
/// waits for a relay lookup.
///
/// A stored level wins over the relay-derived one, but only for a pubkey the relay still
/// counts as a member: a person with no relay role, and a deactivated agent, resolve to
/// `None` whatever is stored. An agent's stored level is capped at its owner's resolved
/// level. An owner chain longer than [`MAX_OWNER_CHAIN`], or one that loops, is a `Conflict`.
pub async fn resolve_actor(
    store: &dyn Store,
    dir: &dyn Directory,
    community: CommunityId,
    pubkey: &str,
) -> Result<Actor, StoreError> {
    // Walk from the pubkey up through its owners.
    let mut chain: Vec<Link> = Vec::new();
    let mut current = pubkey.to_owned();
    loop {
        let agent = dir.agent_link(community, &current).await?;
        if chain.iter().any(|seen| seen.key == current) {
            return Err(StoreError::Conflict("agent owner chain loops".into()));
        }
        let role = match agent {
            Some(_) => None,
            None => dir.relay_role(community, &current).await?,
        };
        let next = agent.as_ref().map(|link| link.owner.clone());
        chain.push(Link {
            key: current.clone(),
            owner: next.clone(),
            deactivated: agent.is_some_and(|link| link.deactivated),
            role,
        });
        match next {
            Some(next) => {
                if chain.len() > MAX_OWNER_CHAIN {
                    return Err(StoreError::Conflict("agent owner chain too long".into()));
                }
                current = next;
            }
            None => break,
        }
    }

    // The stored levels, read in one short transaction.
    let mut stored: Vec<Option<Level>> = Vec::with_capacity(chain.len());
    {
        let mut tx = store.begin(community).await?;
        for link in &chain {
            stored.push(tx.access().level(&link.key).await?.map(|(level, _)| level));
        }
    }

    // Resolve from the top of the chain down, so each agent sees its owner's level.
    let mut owner_level: Option<Level> = None;
    let mut owner_member = true;
    let mut resolved: Option<Actor> = None;
    for (link, stored) in chain.into_iter().zip(stored).rev() {
        let kind = if link.owner.is_some() {
            ActorKind::Agent
        } else {
            ActorKind::Person
        };
        let member = match kind {
            ActorKind::Person => link.role.is_some(),
            ActorKind::Agent => !link.deactivated && owner_member,
        };
        let level = if !member {
            Level::None
        } else {
            match stored {
                Some(level) => owner_level.map_or(level, |ol| cap_agent(level, ol)),
                None => starting_level(kind, link.role),
            }
        };
        owner_level = Some(level);
        owner_member = member;
        resolved = Some(Actor {
            pubkey: link.key,
            kind,
            level,
            owner: link.owner,
            member,
        });
    }
    Ok(resolved.expect("the chain always holds the starting pubkey"))
}
