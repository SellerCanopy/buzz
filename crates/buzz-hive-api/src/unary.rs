//! The wrapper every route uses: check the bearer, decode JSON, run the handler, map the error.

use std::future::Future;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{post, MethodRouter};
use axum::Json;
use buzz_hive_core::access::{allow, ActorKind, Level};
use buzz_hive_core::ports::store::{CommunityId, Pubkey};
use buzz_hive_core::resolve::Actor;

use crate::app::App;
use crate::error::HiveError;

/// The `x-hive-claim` header: a signed statement of who an agent acts for.
pub const CLAIM_HEADER: &str = "x-hive-claim";

/// What an agent may do with no claim: read, never act. A person is never capped this way.
pub const UNCLAIMED_AGENT_LEVEL: Level = Level::Read;

pub struct Ctx {
    pub actor: Actor,
    /// The level of the person an agent acts for, read now. `None` for a person. An agent
    /// with no claim is held to [`UNCLAIMED_AGENT_LEVEL`].
    pub requester: Option<Level>,
    /// The person the claim names, `None` for a person and for an agent with no claim.
    pub requester_pubkey: Option<Pubkey>,
    pub community: CommunityId,
}

impl Ctx {
    /// The caller as the engine's operations and event log see it.
    pub fn op(&self) -> buzz_hive_core::orchestrator::Ctx {
        buzz_hive_core::orchestrator::Ctx {
            community: self.community,
            actor: buzz_hive_core::domain::events::Actor {
                pubkey: self.actor.pubkey.clone(),
                kind: self.actor.kind,
            },
            requester: self.requester_pubkey.clone(),
        }
    }
}

/// A POST route that needs `needs`. The body is Connect JSON; an empty body means `{}`.
///
/// Add it to a router that goes through [`crate::finish`], which caps the body at 16 MiB.
pub fn unary<Req, Res, F, Fut>(needs: Level, f: F) -> MethodRouter<App>
where
    Req: serde::de::DeserializeOwned + Send + 'static,
    Res: serde::Serialize + Send + 'static,
    F: Fn(App, Ctx, Req) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Res, HiveError>> + Send + 'static,
{
    unary_with(needs, false, f)
}

/// [`unary`], and with `sealed` a body that does not decode is refused without the decoder's
/// words. serde repeats the value it met (`invalid type: integer `123``), and the body of a
/// route that carries a secret must not reach a response or a log line that way.
pub fn unary_with<Req, Res, F, Fut>(needs: Level, sealed: bool, f: F) -> MethodRouter<App>
where
    Req: serde::de::DeserializeOwned + Send + 'static,
    Res: serde::Serialize + Send + 'static,
    F: Fn(App, Ctx, Req) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Res, HiveError>> + Send + 'static,
{
    post(
        move |State(app): State<App>, headers: HeaderMap, body: Bytes| {
            let f = f.clone();
            async move { handle(app, needs, sealed, f, headers, body).await }
        },
    )
}

async fn handle<Req, Res, F, Fut>(
    app: App,
    needs: Level,
    sealed: bool,
    f: F,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Res>, HiveError>
where
    Req: serde::de::DeserializeOwned,
    F: Fn(App, Ctx, Req) -> Fut,
    Fut: Future<Output = Result<Res, HiveError>>,
{
    let actor = caller(&app, &headers).await?;
    let (requester, requester_pubkey) = requester_of(&app, &actor, &headers).await?;
    let ctx = Ctx {
        actor,
        requester,
        requester_pubkey,
        community: app.community,
    };
    allow(ctx.actor.level, ctx.requester, needs).map_err(|d| {
        HiveError::PermissionDenied(format!("needs {:?}, you have {:?}", d.needed, d.effective))
    })?;
    let bytes: &[u8] = if body.is_empty() { b"{}" } else { &body };
    let request: Req = serde_json::from_slice(bytes).map_err(|e| {
        let said = e.to_string();
        // A missing field names the field and holds no value. Anything else may repeat one.
        if sealed && e.is_data() && !said.starts_with("missing field") {
            return HiveError::InvalidArgument(
                "invalid request body: a field has the wrong type (the body is not shown)".into(),
            );
        }
        if sealed && !e.is_data() {
            return HiveError::InvalidArgument(
                "invalid request body: it is not JSON (the body is not shown)".into(),
            );
        }
        HiveError::InvalidArgument(format!("invalid request body: {said}"))
    })?;
    f(app, ctx, request).await.map(Json)
}

/// The actor behind the request's bearer token, at the level it holds now.
pub(crate) async fn caller(app: &App, headers: &HeaderMap) -> Result<Actor, HiveError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| HiveError::Unauthenticated("missing bearer token".into()))?;
    let claims = app.minter.verify(token)?;
    if claims.community != app.community {
        return Err(HiveError::Unauthenticated(
            "token is for another community".into(),
        ));
    }
    app.resolve(&claims.sub).await
}

/// The person the caller acts for and their level. A person acts for themselves. An agent
/// acts for the person its claim names, at that person's level now, so a person who lost
/// access takes the agent's reach with them. An agent with no claim is held to
/// [`UNCLAIMED_AGENT_LEVEL`]. A claim that is forged, expired, for another agent or for
/// another community is refused outright.
async fn requester_of(
    app: &App,
    actor: &Actor,
    headers: &HeaderMap,
) -> Result<(Option<Level>, Option<Pubkey>), HiveError> {
    if actor.kind != ActorKind::Agent {
        return Ok((None, None));
    }
    let Some(header) = headers.get(CLAIM_HEADER) else {
        return Ok((Some(UNCLAIMED_AGENT_LEVEL), None));
    };
    let claim = app.minter.verify_claim(
        header
            .to_str()
            .map_err(|_| HiveError::Unauthenticated("invalid requester claim".into()))?,
    )?;
    if claim.community != app.community || claim.agent != actor.pubkey {
        return Err(HiveError::Unauthenticated(
            "requester claim was issued to another agent".into(),
        ));
    }
    // A claim that names an agent is refused even if one was signed: an agent never acts at
    // another agent's level, or its own, through a claim.
    let requester = app.resolve(&claim.sub).await?;
    if requester.kind != ActorKind::Person {
        return Err(HiveError::Unauthenticated(
            "requester claim names an agent, not a person".into(),
        ));
    }
    // A claim minted from a run lasts only while the run still holds this agent's stage, so
    // an agent that handed the run on cannot keep acting for the person who started it.
    if let Some(run) = claim.via.strip_prefix("run:") {
        match crate::claims::requester_of_run(app, &actor.pubkey, run).await {
            Ok(person) if person == claim.sub => {}
            // The store being down says nothing about the run, so it is not a refusal.
            Err(error) if error.status().is_server_error() => return Err(error),
            _ => {
                return Err(HiveError::Unauthenticated(
                    "requester claim is for a run that no longer holds this agent's stage".into(),
                ))
            }
        }
    }
    Ok((Some(requester.level), Some(claim.sub)))
}
