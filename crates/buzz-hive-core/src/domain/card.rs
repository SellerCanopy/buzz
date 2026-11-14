//! A card: one piece of work on the board.

use uuid::Uuid;

/// Where a card is in its life. The stored text is the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    Idle,
    Running,
    Waiting,
    Failed,
    Done,
    Stopped,
}

impl CardState {
    /// The value stored in `hive.cards.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Done => "done",
            Self::Stopped => "stopped",
        }
    }

    /// The inverse of [`CardState::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "running" => Some(Self::Running),
            "waiting" => Some(Self::Waiting),
            "failed" => Some(Self::Failed),
            "done" => Some(Self::Done),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub state: CardState,
    /// The workflow package name; empty for a card in Backlog.
    pub workflow: String,
    pub archived: bool,
    /// Pubkey of the person or agent that created it.
    pub created_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_its_stored_name() {
        for state in [
            CardState::Idle,
            CardState::Running,
            CardState::Waiting,
            CardState::Failed,
            CardState::Done,
            CardState::Stopped,
        ] {
            assert_eq!(CardState::parse(state.as_str()), Some(state));
        }
        assert_eq!(CardState::parse("paused"), None);
    }
}
