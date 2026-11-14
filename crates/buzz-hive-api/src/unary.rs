//! The wrapper every route uses: check the bearer, decode JSON, run the handler, map the error.

use std::future::Future;

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::{post, MethodRouter};
use axum::Json;
use buzz_hive_core::access::{allow, Level};
use buzz_hive_core::ports::store::CommunityId;
use buzz_hive_core::resolve::Actor;

use crate::app::App;
use crate::error::HiveError;

pub struct Ctx {
    pub actor: Actor,
    /// The level of the person an agent acts for. `None` until requester claims exist (Phase 15).
    pub requester: Option<Level>,
    pub community: CommunityId,
}

/// A POST route that needs `needs`. The body is Connect JSON; an empty body means `{}`.
///
/// Add it to a router that goes through [`crate::finish`], which caps the body at 16 MiB.
pub fn unary<S, Req, Res, F, Fut>(app: App, needs: Level, f: F) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Req: serde::de::DeserializeOwned + Send + 'static,
    Res: serde::Serialize + Send + 'static,
    F: Fn(App, Ctx, Req) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Res, HiveError>> + Send + 'static,
{
    post(move |headers: HeaderMap, body: Bytes| {
        let app = app.clone();
        let f = f.clone();
        async move { handle(app, needs, f, headers, body).await }
    })
}

async fn handle<Req, Res, F, Fut>(
    app: App,
    needs: Level,
    f: F,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Res>, HiveError>
where
    Req: serde::de::DeserializeOwned,
    F: Fn(App, Ctx, Req) -> Fut,
    Fut: Future<Output = Result<Res, HiveError>>,
{
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
    let actor = app.resolve(&claims.sub).await?;
    let ctx = Ctx {
        actor,
        requester: None,
        community: app.community,
    };
    allow(ctx.actor.level, ctx.requester, needs).map_err(|d| {
        HiveError::PermissionDenied(format!("needs {:?}, you have {:?}", d.needed, d.effective))
    })?;
    let bytes: &[u8] = if body.is_empty() { b"{}" } else { &body };
    let request: Req = serde_json::from_slice(bytes)
        .map_err(|e| HiveError::InvalidArgument(format!("invalid request body: {e}")))?;
    f(app, ctx, request).await.map(Json)
}
