//! Access levels: the one rule every route and tool uses.

/// What a pubkey may do in Beehive. Ordered: each level includes the ones below.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    None = 0,
    Read = 1,
    Run = 2,
    Admin = 3,
}

impl Level {
    /// The value stored in `hive.access_levels.level`.
    pub fn as_i16(self) -> i16 {
        self as i16
    }

    /// The lowercase name shown to people and sent to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Run => "run",
            Self::Admin => "admin",
        }
    }

    /// The inverse of [`Level::as_i16`]. `None` for a value outside 0 to 3.
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Read),
            2 => Some(Self::Run),
            3 => Some(Self::Admin),
            _ => None,
        }
    }
}

/// A level that was refused, with the level the action needed and the level in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denied {
    pub needed: Level,
    pub effective: Level,
}

/// Whether a pubkey belongs to a person or to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    Person,
    Agent,
}

impl ActorKind {
    /// The value stored in `hive.access_levels.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
        }
    }

    /// The inverse of [`ActorKind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "person" => Some(Self::Person),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// A pubkey's role in the relay's member list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayRole {
    Owner,
    Admin,
    Member,
}

/// The level a pubkey has before an admin sets one.
/// A person is Admin when the relay says owner or admin, Read for a member, None otherwise.
/// An agent starts at None. A stored agent level is capped by `cap_agent`.
pub fn starting_level(kind: ActorKind, role: Option<RelayRole>) -> Level {
    match (kind, role) {
        (ActorKind::Person, Some(RelayRole::Owner | RelayRole::Admin)) => Level::Admin,
        (ActorKind::Person, Some(RelayRole::Member)) => Level::Read,
        (ActorKind::Person, None) => Level::None,
        (ActorKind::Agent, _) => Level::None,
    }
}

/// An agent's stored level is capped at its owner's level.
pub fn cap_agent(stored: Level, owner: Level) -> Level {
    stored.min(owner)
}

/// The level an actor acts at: its own, or its requester's when that is lower.
pub fn effective(actor: Level, requester: Option<Level>) -> Level {
    requester.map_or(actor, |r| actor.min(r))
}

/// Allow an action that needs `needed`, or say which levels fell short.
pub fn allow(actor: Level, requester: Option<Level>, needed: Level) -> Result<(), Denied> {
    let effective = effective(actor, requester);
    if effective >= needed {
        Ok(())
    } else {
        Err(Denied { needed, effective })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALL: [Level; 4] = [Level::None, Level::Read, Level::Run, Level::Admin];

    #[test]
    fn every_actor_requester_needed_triple() {
        for actor in ALL {
            for req in ALL {
                for needed in ALL {
                    let want = actor.min(req) >= needed;
                    assert_eq!(
                        allow(actor, Some(req), needed).is_ok(),
                        want,
                        "{actor:?} {req:?} {needed:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn no_requester_uses_actor_level() {
        for actor in ALL {
            for needed in ALL {
                assert_eq!(allow(actor, None, needed).is_ok(), actor >= needed);
            }
        }
    }

    #[test]
    fn denial_names_both_levels() {
        assert_eq!(
            allow(Level::Admin, Some(Level::Read), Level::Run),
            Err(Denied {
                needed: Level::Run,
                effective: Level::Read
            })
        );
    }

    #[test]
    fn starting_levels() {
        use {ActorKind::*, RelayRole::*};
        assert_eq!(starting_level(Person, Some(Owner)), Level::Admin);
        assert_eq!(starting_level(Person, Some(Admin)), Level::Admin);
        assert_eq!(starting_level(Person, Some(Member)), Level::Read);
        assert_eq!(starting_level(Person, None), Level::None);
        assert_eq!(starting_level(Agent, Some(Member)), Level::None);
    }

    #[test]
    fn agent_never_above_owner() {
        assert_eq!(cap_agent(Level::Admin, Level::Read), Level::Read);
    }

    #[test]
    fn storage_encodings_round_trip() {
        for level in ALL {
            assert_eq!(Level::from_i16(level.as_i16()), Some(level));
        }
        assert_eq!(Level::from_i16(4), None);
        assert_eq!(Level::from_i16(-1), None);
        for kind in [ActorKind::Person, ActorKind::Agent] {
            assert_eq!(ActorKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ActorKind::parse("robot"), None);
    }
}
