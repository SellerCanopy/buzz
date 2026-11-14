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
