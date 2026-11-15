//! `POST /auth/token`: trade a signed NIP-98 event for a Beehive token.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use base64::Engine as _;

use crate::app::App;
use crate::error::HiveError;
use crate::replay::Seen;

#[derive(serde::Serialize)]
pub struct TokenReply {
    pub token: String,
    /// Seconds since the epoch.
    pub expires_at: i64,
}

pub async fn token(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<TokenReply>, HiveError> {
    let header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Nostr "))
        .ok_or_else(|| HiveError::Unauthenticated("missing Nostr authorization".into()))?;
    let event_json = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(header)
            .map_err(|_| HiveError::Unauthenticated("authorization is not base64".into()))?,
    )
    .map_err(|_| HiveError::Unauthenticated("authorization is not text".into()))?;

    let expected = format!(
        "{}{}",
        app.config.public_url.trim_end_matches('/'),
        crate::AUTH_TOKEN_PATH
    );
    let pubkey =
        buzz_auth::verify_nip98_event(&event_json, &expected, "POST", None).map_err(|e| {
            tracing::warn!(error = %e, "sign-in refused: NIP-98 verification failed");
            HiveError::Unauthenticated("authorization was not accepted".into())
        })?;
    let hex = pubkey.to_hex();
    let event_id = serde_json::from_str::<serde_json::Value>(&event_json)
        .ok()
        .and_then(|v| v["id"].as_str().map(str::to_owned))
        .ok_or_else(|| HiveError::Unauthenticated("event has no id".into()))?;

    // Only a verified event costs a lookup, and only this many a second. Anyone can sign an
    // event, so without a ceiling the relay and the store answer for every one of them.
    if !app.auth_limit.try_acquire(Instant::now()) {
        tracing::warn!(pubkey = %hex, "sign-in refused: rate limit");
        return Err(HiveError::ResourceExhausted(
            "too many sign-ins, try again shortly".into(),
        ));
    }

    // Membership first, replay second: an event signed by a stranger must not take a slot in
    // the seen-set, or strangers could fill it and lock real members out.
    let actor = app.resolve(&hex).await?;
    if !actor.member {
        tracing::warn!(pubkey = %hex, "sign-in refused: not a member");
        return Err(HiveError::PermissionDenied(
            "not a member of this community".into(),
        ));
    }
    match app.replay.first_use(&event_id, Instant::now()) {
        Seen::New => {}
        Seen::Replayed => {
            tracing::warn!(pubkey = %hex, event = %short(&event_id), "sign-in refused: replayed event");
            return Err(HiveError::Unauthenticated(
                "authorization already used".into(),
            ));
        }
        Seen::Full => {
            tracing::warn!(pubkey = %hex, event = %short(&event_id), "sign-in refused: replay guard is full");
            return Err(HiveError::Unavailable(
                "too many sign-ins in flight, try again shortly".into(),
            ));
        }
    }

    let (token, expires_at) = app.minter.mint(&hex, app.community, actor.owner)?;
    tracing::info!(pubkey = %hex, level = ?actor.level, "sign-in accepted");
    Ok(Json(TokenReply { token, expires_at }))
}

/// The first characters of an event id, enough to find it in a log.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}
