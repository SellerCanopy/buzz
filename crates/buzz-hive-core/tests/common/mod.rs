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
    let files = MemSource::new()
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
