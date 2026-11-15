//! Who may do what: the access page's list, and an admin changing a level.
//!
//! The list shows the level each person and agent acts at today, which is what
//! [`crate::resolve::resolve_actor`] would give them: a stored level wins over the one the
//! relay's role gives, and an agent never acts above its owner. `set_level` refuses a level
//! an agent's owner could not use, so the page never shows a level that does not hold.

use std::collections::HashMap;

use serde_json::json;

use super::{Ctx, Env, OpError};
use crate::access::{cap_agent, starting_level, ActorKind, Level};
use crate::domain::events::Event;
use crate::ports::store::{Pubkey, StoreError};
use crate::resolve::{resolve_actor, Actor, MAX_OWNER_CHAIN};

const KIND: &str = "access.changed";

/// A person or agent and the level it acts at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEntry {
    pub pubkey: Pubkey,
    /// The display name the relay holds, empty when there is none.
    pub name: String,
    pub kind: ActorKind,
    /// The agent's owner, `None` for a person.
    pub owner: Option<Pubkey>,
    /// The level it acts at now, after the owner's cap.
    pub level: Level,
}

/// Every relay member and every active agent with the level it acts at. People come first,
/// then agents, each by name.
///
/// The relay is asked before the store is opened, so no store connection waits on it.
pub async fn list(env: &Env<'_>, ctx: &Ctx) -> Result<Vec<AccessEntry>, OpError> {
    let people = env.directory.people(ctx.community).await?;
    let agents = env.directory.list_agents(ctx.community).await?;
    let stored: HashMap<Pubkey, Level> = {
        let mut tx = env.store.begin(ctx.community).await?;
        tx.access()
            .list()
            .await?
            .into_iter()
            .map(|row| (row.pubkey, row.level))
            .collect()
    };

    let mut entries: Vec<AccessEntry> = people
        .into_iter()
        .map(|person| {
            let level = stored
                .get(&person.pubkey)
                .copied()
                .unwrap_or_else(|| starting_level(ActorKind::Person, Some(person.role)));
            AccessEntry {
                name: person.name.unwrap_or_default(),
                pubkey: person.pubkey,
                kind: ActorKind::Person,
                owner: None,
                level,
            }
        })
        .collect();
    entries.sort_by_key(|e| (e.name.to_lowercase(), e.pubkey.clone()));

    let by_key: HashMap<Pubkey, (Level, Option<Pubkey>)> = entries
        .iter()
        .map(|e| (e.pubkey.clone(), (e.level, None)))
        .chain(agents.iter().map(|a| {
            (
                a.pubkey.clone(),
                (
                    stored.get(&a.pubkey).copied().unwrap_or(Level::None),
                    a.owner.clone(),
                ),
            )
        }))
        .collect();
    // An agent acts at its stored level or its owner's, whichever is lower. An owner the
    // relay does not list is no member, so its agents act at None.
    let capped = |agent: &Pubkey| -> Level {
        let mut level = Level::Admin;
        let mut current = agent.clone();
        for _ in 0..=MAX_OWNER_CHAIN {
            match by_key.get(&current) {
                Some((own, Some(owner))) => {
                    level = cap_agent(level, *own);
                    current = owner.clone();
                }
                Some((own, None)) => return cap_agent(level, *own),
                None => return Level::None,
            }
        }
        // A chain this long is refused when the agent acts; the page shows None.
        Level::None
    };
    for agent in &agents {
        entries.push(AccessEntry {
            pubkey: agent.pubkey.clone(),
            name: agent.name.clone(),
            kind: ActorKind::Agent,
            owner: agent.owner.clone(),
            level: capped(&agent.pubkey),
        });
    }
    Ok(entries)
}

/// Sets the level of a person or agent in this community, and returns its entry as it now
/// stands with the events committed.
///
/// A person may not lower their own level: an admin who did could leave no one able to undo
/// it. An agent's level may not exceed its owner's, which names both in the refusal. Whether
/// the caller may set levels at all is the route's check, and whether an agent caller may is
/// the route's too, since only it knows the requester.
pub async fn set_level(
    env: &Env<'_>,
    ctx: &Ctx,
    target: &str,
    level: Level,
) -> Result<(AccessEntry, Vec<Event>), OpError> {
    // Everything the relay says comes first: the log lock is taken by the first append.
    let agent = env.directory.agent_link(ctx.community, target).await?;
    let (kind, owner, role) = match &agent {
        Some(link) => (ActorKind::Agent, Some(link.owner.clone()), None),
        None => (
            ActorKind::Person,
            None,
            env.directory.relay_role(ctx.community, target).await?,
        ),
    };
    if kind == ActorKind::Person && role.is_none() {
        return Err(OpError::NotFound(format!(
            "{target} is not a member of this community"
        )));
    }
    if agent.as_ref().is_some_and(|link| link.deactivated) {
        return Err(OpError::FailedPrecondition(
            "this agent is switched off in Buzz".into(),
        ));
    }
    if target == ctx.actor.pubkey && level < Level::Admin {
        return Err(OpError::FailedPrecondition(
            "you cannot lower your own level; ask another admin".into(),
        ));
    }
    let people = env.directory.people(ctx.community).await?;
    let name_of = |pubkey: &str| -> Option<String> {
        people
            .iter()
            .find(|p| p.pubkey == pubkey)
            .and_then(|p| p.name.clone())
    };
    let name = match kind {
        ActorKind::Person => name_of(target).unwrap_or_default(),
        ActorKind::Agent => env
            .directory
            .list_agents(ctx.community)
            .await?
            .into_iter()
            .find(|a| a.pubkey == target)
            .map(|a| a.name)
            .unwrap_or_default(),
    };
    let previous = acts_at(env, ctx, target).await?.level;
    if let Some(owner) = &owner {
        let owner_level = acts_at(env, ctx, owner).await?.level;
        if level > owner_level {
            let who = if name.is_empty() { target } else { &name };
            let owner_name = name_of(owner).filter(|n| !n.is_empty());
            return Err(OpError::FailedPrecondition(format!(
                "agent {who} is capped at {} by its owner {}",
                owner_level.as_str(),
                owner_name.as_deref().unwrap_or(owner),
            )));
        }
    }

    let mut tx = env.store.begin(ctx.community).await?;
    tx.access()
        .set_level(target, level, kind, &ctx.actor.pubkey)
        .await?;
    tx.events()
        .append(Event::new(
            KIND,
            None,
            None,
            ctx.actor.clone(),
            json!({
                "pubkey": target,
                "kind": kind.as_str(),
                "level": level.as_str(),
                "previous": previous.as_str(),
            }),
        ))
        .await?;
    let events = tx.commit().await?;
    Ok((
        AccessEntry {
            pubkey: target.to_owned(),
            name,
            kind,
            owner,
            level,
        },
        events,
    ))
}

/// What `pubkey` acts at today. A corrupt owner chain is a failed precondition.
async fn acts_at(env: &Env<'_>, ctx: &Ctx, pubkey: &str) -> Result<Actor, OpError> {
    resolve_actor(env.store, env.directory, ctx.community, pubkey)
        .await
        .map_err(|e| match e {
            StoreError::Conflict(message) => OpError::FailedPrecondition(message),
            other => OpError::Store(other),
        })
}
