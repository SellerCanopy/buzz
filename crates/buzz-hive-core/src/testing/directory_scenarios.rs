//! Scenarios for the relay read that decides whether a requester claim is stale, so the
//! in-memory directory and the Postgres one are held to one rule. Each implementation's own
//! test file seeds a scenario's events, asks [`newer_mention`](crate::ports::store::Directory)
//! and compares the answer with `expect`.

use uuid::Uuid;

use crate::ports::store::RelayEvent;

/// The channel every scenario posts in, unless it says otherwise.
pub const CHANNEL: Uuid = Uuid::from_u128(5);
/// A channel the scenarios that cross channels post in.
pub const OTHER_CHANNEL: Uuid = Uuid::from_u128(6);
/// Seconds since the epoch of the scenario's first message.
pub const START: i64 = 1_700_000_000;

/// An id of 64 hex characters, `n` repeated.
pub fn id(n: u8) -> String {
    hex::encode([n; 32])
}

/// The agent asked, and the thread's root message.
pub fn agent() -> String {
    id(0xa1)
}
pub fn root() -> String {
    id(0xc3)
}

/// One question: with these messages, does a newer one than `subject` mention the agent in the
/// thread of `root`?
#[derive(Debug, Clone)]
pub struct NewerMention {
    pub name: &'static str,
    pub events: Vec<RelayEvent>,
    /// The id of the message in `events` whose freshness is asked.
    pub subject: String,
    pub root: String,
    pub expect: bool,
}

impl NewerMention {
    /// The message the question is about.
    pub fn subject(&self) -> &RelayEvent {
        self.events
            .iter()
            .find(|e| e.id == self.subject)
            .expect("the subject is one of the events")
    }
}

fn tags(channel: Uuid, agent: &str, thread: &[&str]) -> Vec<Vec<String>> {
    let mut tags = vec![
        vec!["h".to_owned(), channel.to_string()],
        vec!["p".to_owned(), agent.to_owned()],
    ];
    tags.extend(
        thread
            .iter()
            .map(|t| t.split(',').map(str::to_owned).collect()),
    );
    tags
}

fn message(n: u8, at: i64, tags: Vec<Vec<String>>) -> RelayEvent {
    RelayEvent {
        id: id(n),
        author: id(0xb2),
        kind: 9,
        tags,
        created_at: at,
        channel_id: Some(CHANNEL),
        deleted: false,
    }
}

/// A message that mentions the agent in the thread of the root, marked `root`.
fn in_thread(n: u8, at: i64) -> RelayEvent {
    let root = root();
    message(
        n,
        at,
        tags(CHANNEL, &agent(), &[&format!("e,{root},,root")]),
    )
}

fn ask(name: &'static str, events: Vec<RelayEvent>, subject: u8, expect: bool) -> NewerMention {
    NewerMention {
        name,
        events,
        subject: id(subject),
        root: root(),
        expect,
    }
}

/// Every scenario. The subject is message 1 unless a scenario says otherwise.
pub fn newer_mention_scenarios() -> Vec<NewerMention> {
    let first = || in_thread(1, START);
    let other_root = id(0xd4);
    let named_in = |n: u8, at: i64, thread: &str| {
        message(n, at, tags(CHANNEL, &agent(), &[&format!("e,{thread}")]))
    };
    let mut cases = vec![
        ask("nothing newer", vec![first()], 1, false),
        ask(
            "a newer message in the thread mentions the agent",
            vec![first(), in_thread(2, START + 10)],
            1,
            true,
        ),
        ask(
            "an older message does not count",
            vec![first(), in_thread(2, START - 10)],
            1,
            false,
        ),
        ask(
            "same second, the higher id is newer",
            vec![first(), in_thread(2, START)],
            1,
            true,
        ),
        ask(
            "same second, the lower id is older",
            vec![in_thread(1, START), in_thread(2, START)],
            2,
            false,
        ),
    ];

    let mut reaction = in_thread(2, START + 10);
    reaction.kind = 7;
    cases.push(ask(
        "a reaction is not a message",
        vec![first(), reaction],
        1,
        false,
    ));

    let mut deleted = in_thread(2, START + 10);
    deleted.deleted = true;
    cases.push(ask(
        "a deleted message does not count",
        vec![first(), deleted],
        1,
        false,
    ));

    let mut elsewhere = in_thread(2, START + 10);
    elsewhere.channel_id = Some(OTHER_CHANNEL);
    cases.push(ask(
        "a message in another channel does not count",
        vec![first(), elsewhere],
        1,
        false,
    ));

    cases.push(ask(
        "a message in another thread does not count",
        vec![
            first(),
            message(
                2,
                START + 10,
                tags(CHANNEL, &agent(), &[&format!("e,{other_root},,root")]),
            ),
        ],
        1,
        false,
    ));
    cases.push(ask(
        "a message that mentions someone else does not count",
        vec![
            first(),
            message(
                2,
                START + 10,
                tags(CHANNEL, &id(0xa2), &[&format!("e,{},,root", root())]),
            ),
        ],
        1,
        false,
    ));
    cases.push(ask(
        "a reply marked reply counts",
        vec![
            first(),
            named_in(2, START + 10, &format!("{},,reply", root())),
        ],
        1,
        true,
    ));
    cases.push(ask(
        "a reply whose e tag has no marker counts",
        vec![first(), named_in(2, START + 10, &root())],
        1,
        true,
    ));

    // The first message of a thread names no root: it is the root.
    let opener = message(1, START, tags(CHANNEL, &agent(), &[]));
    let mut under_it = in_thread(2, START + 10);
    under_it.tags = tags(CHANNEL, &agent(), &[&format!("e,{},,root", id(1))]);
    cases.push(NewerMention {
        name: "replies to a thread's first message name its id",
        events: vec![opener.clone(), under_it],
        subject: id(1),
        root: id(1),
        expect: true,
    });
    cases.push(NewerMention {
        name: "a thread's first message with no replies has nothing newer",
        events: vec![opener, in_thread(2, START + 10)],
        subject: id(1),
        root: id(1),
        expect: false,
    });
    cases
}
