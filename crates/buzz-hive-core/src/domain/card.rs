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

/// The longest slug of a title in a card branch, in characters.
const BRANCH_SLUG_CHARS: usize = 40;

/// The git branch a card's work goes on: `buzz/<first 8 characters of the id>-<slug of the
/// title>`. The id part keeps two cards with one title apart, and the slug lets a person
/// reading the repo's branch list tell what the branch is for. A title with no letters or
/// digits leaves the id alone. The name is the same every time it is asked for, so a stage
/// that runs again pushes to the branch the stage before it did.
pub fn card_branch(id: Uuid, title: &str) -> String {
    let short = &id.simple().to_string()[..8];
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= BRANCH_SLUG_CHARS {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("buzz/{short}")
    } else {
        format!("buzz/{short}-{slug}")
    }
}

/// The pull request a card's `open_pull_request` stage opened. One row per card: a second
/// visit to the stage finds the open pull request for the same branch and keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequest {
    /// The repo as `owner/name`.
    pub repo: String,
    pub number: u64,
    pub url: String,
    /// The card branch the pull request was opened from.
    pub head: String,
    pub base: String,
    /// `open`, `closed` or `merged`.
    pub state: String,
    pub draft: bool,
    /// A reviewer approved it and none has asked for changes since.
    pub approved: bool,
    /// How its checks stand as last read: empty when none were read, else `pending`, `passed`
    /// or `failed`.
    pub checks: String,
    /// The head commit `checks` were read for, or empty when none were read. A merge the
    /// workflow makes is pinned to it, so a push after the read is refused, not merged unchecked.
    pub head_sha: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_branch_is_the_short_id_and_a_slug_of_the_title() {
        let id = Uuid::parse_str("0a1b2c3d-0000-0000-0000-000000000000").unwrap();
        assert_eq!(
            card_branch(id, "Fix: the  crash on SAVE!"),
            "buzz/0a1b2c3d-fix-the-crash-on-save"
        );
        assert_eq!(card_branch(id, "  ... "), "buzz/0a1b2c3d");
        assert_eq!(card_branch(id, "Über"), "buzz/0a1b2c3d-ber");
        let long = card_branch(id, &"word ".repeat(30));
        assert!(
            long.len() <= "buzz/0a1b2c3d-".len() + BRANCH_SLUG_CHARS,
            "{long}"
        );
        assert!(!long.ends_with('-'));
        for title in ["", "a/b", "x.lock", "--", "é", "a..b", "ends with dot."] {
            assert!(
                crate::workflow::names::valid_branch_name(&card_branch(id, title)),
                "{title}"
            );
        }
    }

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
