#![allow(dead_code)]

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::events::Actor;
use buzz_hive_core::orchestrator::Ctx;
use buzz_hive_core::ports::store::CommunityId;

pub const COMMUNITY: CommunityId = CommunityId::from_u128(1);

pub fn ctx_for(pubkey: &str, kind: ActorKind) -> Ctx {
    Ctx {
        community: COMMUNITY,
        actor: Actor {
            pubkey: pubkey.into(),
            kind,
        },
    }
}

pub fn person() -> Ctx {
    ctx_for("aa", ActorKind::Person)
}

pub fn agent() -> Ctx {
    ctx_for("bb", ActorKind::Agent)
}

use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::testing::{FakeDirectory, FakeRelay, MemStore};
use buzz_hive_core::workflow::load::{load, Catalog};
use buzz_hive_core::workflow::source::{MemSource, Source};

pub const HIVE: &str = "ee";
pub const BUILDER: &str = "b1";
pub const CHANNEL: uuid::Uuid = uuid::Uuid::from_u128(77);

/// A package whose first stage names the agent `builder` and takes the parameter `depth`.
pub fn catalog() -> Catalog {
    let mut files = MemSource::new()
        .with(
            "demo/kiln-package.yaml",
            "name: demo\nversion: 1.0.0\nparameters:\n  - name: depth\n    type: choice\n    choices: [normal, deep]\n    default: normal\n",
        )
        .with(
            "demo/workflow.yaml",
            "version: 2\nstages:\n  - id: research\n    name: Research\n    kind: prompt\n    agent: builder\n    prompt: \"Research {{issue.title}} at depth {{params.depth}}\"\n  - id: build\n    kind: prompt\n    agent: builder\n    prompt: Build\n",
        )
        .with("solo/kiln-package.yaml", "name: solo\nversion: 1.0.0\n")
        .with(
            "solo/workflow.yaml",
            "version: 2\nstages:\n  - id: only\n    kind: prompt\n    agent: builder\n    prompt: Do {{issue.title}}\n",
        )
        .with(
            "noagent/kiln-package.yaml",
            "name: noagent\nversion: 1.0.0\n",
        )
        .with(
            "noagent/workflow.yaml",
            "version: 2\nstages:\n  - id: a\n    kind: prompt\n    prompt: Do it\n",
        );
    // Workflows that route a card. Each stage is a prompt stage worked by `builder`.
    let stage = |id: &str, extra: &str| {
        format!("  - id: {id}\n    kind: prompt\n    agent: builder\n    prompt: Do {id}\n{extra}")
    };
    let routed = [
        (
            "three",
            [stage("a", ""), stage("b", ""), stage("c", "")].concat(),
        ),
        // A repeat stage whose rounds each open a new thread.
        (
            "fresh",
            stage(
                "work",
                "    repeat:\n      until: agent_done\n      fresh_session: true\n",
            ),
        ),
        // A repeat stage whose rounds reply in its one thread.
        (
            "plain",
            stage(
                "work",
                "    repeat:\n      until: agent_done\n      max_rounds: 3\n",
            ),
        ),
        // The card waits before b until a person resumes it.
        (
            "held",
            [stage("a", ""), stage("b", "    pause: true\n")].concat(),
        ),
        // b needs a label the card does not have, and goes to c without it.
        (
            "cond",
            [
                stage("a", ""),
                stage(
                    "b",
                    "    conditions:\n      labels: [frontend]\n      else: c\n",
                ),
                stage("c", ""),
            ]
            .concat(),
        ),
        // b sends a failure back to a, once.
        (
            "loop",
            [
                stage("a", ""),
                stage("b", "    on:\n      failed: a\n    loop_limit: 1\n"),
            ]
            .concat(),
        ),
        // Where a finished card can be handed on to.
        ("second", stage("s1", "")),
    ];
    for (name, stages) in routed {
        files = files
            .with(
                &format!("{name}/kiln-package.yaml"),
                format!("name: {name}\nversion: 1.0.0\n"),
            )
            .with(
                &format!("{name}/workflow.yaml"),
                format!("version: 2\nstages:\n{stages}"),
            );
    }
    load(&[Source::new("test", false, files)])
}

/// Everything an operation needs, with Beehive and `builder` in the update channel.
pub struct Fixture {
    pub store: MemStore,
    pub directory: FakeDirectory,
    pub catalog: Catalog,
    pub relay: FakeRelay,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            store: MemStore::new(),
            directory: FakeDirectory::new()
                .with_named_agent("builder", BUILDER, "aa")
                .with_channel(CHANNEL, &[HIVE, BUILDER]),
            catalog: catalog(),
            relay: FakeRelay::new(),
        }
    }

    pub fn with_directory(mut self, directory: FakeDirectory) -> Self {
        self.directory = directory;
        self
    }

    pub fn env(&self) -> Env<'_> {
        Env {
            store: &self.store,
            directory: &self.directory,
            catalog: &self.catalog,
            hive: HIVE,
        }
    }
}
