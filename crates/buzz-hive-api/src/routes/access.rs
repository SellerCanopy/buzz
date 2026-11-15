//! Who may do what: the people and agents with their levels, and an admin changing one.

use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::orchestrator::access::{self, AccessEntry};

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AccessEntry as WireEntry, ListAccessRequest, ListAccessResponse, ListPeopleRequest,
    ListPeopleResponse, Person, SetAccessLevelRequest,
};
use crate::unary::Ctx;

fn wire(entry: AccessEntry) -> WireEntry {
    WireEntry {
        pubkey: entry.pubkey,
        name: entry.name,
        kind: entry.kind.as_str().to_owned(),
        owner_pubkey: entry.owner.unwrap_or_default(),
        level: entry.level.as_str().to_owned(),
    }
}

pub async fn list_access(
    app: App,
    ctx: Ctx,
    _req: ListAccessRequest,
) -> Result<ListAccessResponse, HiveError> {
    let op = app.op(false)?;
    let entries = access::list(&op.env(), &ctx.op()).await?;
    Ok(ListAccessResponse {
        entries: entries.into_iter().map(wire).collect(),
    })
}

/// The community's people with the level each acts at. Agents are in `ListAgents` and
/// `ListAccess`.
pub async fn list_people(
    app: App,
    ctx: Ctx,
    _req: ListPeopleRequest,
) -> Result<ListPeopleResponse, HiveError> {
    let op = app.op(false)?;
    let entries = access::list(&op.env(), &ctx.op()).await?;
    Ok(ListPeopleResponse {
        people: entries
            .into_iter()
            .filter(|entry| entry.kind == ActorKind::Person)
            .map(|entry| Person {
                user_id: entry.pubkey,
                name: entry.name,
                level: entry.level.as_str().to_owned(),
                ..Default::default()
            })
            .collect(),
    })
}

pub async fn set_access_level(
    app: App,
    ctx: Ctx,
    req: SetAccessLevelRequest,
) -> Result<WireEntry, HiveError> {
    may_set_levels(&ctx)?;
    let pubkey = req.pubkey.trim().to_lowercase();
    if pubkey.is_empty()
        || pubkey.len() > 64
        || !pubkey.len().is_multiple_of(2)
        || !pubkey.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(HiveError::InvalidArgument(
            "pubkey must be hex characters, 64 for a real key".into(),
        ));
    }
    let level = Level::parse(&req.level).ok_or_else(|| {
        HiveError::InvalidArgument(format!(
            "unknown level {:?}; use none, read, run or admin",
            req.level
        ))
    })?;
    let op = app.op(false)?;
    let (entry, events) = access::set_level(&op.env(), &ctx.op(), &pubkey, level).await?;
    op.settle(&events);
    Ok(wire(entry))
}

/// A person at Admin may set levels. An agent may only when the person it acts for is at
/// Admin, which the requester claim says. An agent with no claim is refused: a level an
/// agent grants outlives the request that asked for it.
fn may_set_levels(ctx: &Ctx) -> Result<(), HiveError> {
    if ctx.actor.kind != ActorKind::Agent
        || ctx.requester.is_some_and(|level| level >= Level::Admin)
    {
        return Ok(());
    }
    Err(HiveError::PermissionDenied(
        "an agent may set a level only for a person at Admin; no person is named for this request"
            .into(),
    ))
}
