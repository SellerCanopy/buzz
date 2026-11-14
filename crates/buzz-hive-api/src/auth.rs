//! `POST /auth/token`: trade a signed NIP-98 event for a Beehive token.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use base64::Engine as _;
use buzz_hive_core::access::Level;

use crate::app::App;
use crate::error::HiveError;

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

    let expected = format!("{}/auth/token", app.config.public_url.trim_end_matches('/'));
    let pubkey =
        buzz_auth::verify_nip98_event(&event_json, &expected, "POST", None).map_err(|e| {
            tracing::debug!(error = %e, "NIP-98 verification failed");
            HiveError::Unauthenticated("authorization was not accepted".into())
        })?;
    let event_id = serde_json::from_str::<serde_json::Value>(&event_json)
        .ok()
        .and_then(|v| v["id"].as_str().map(str::to_owned))
        .ok_or_else(|| HiveError::Unauthenticated("event has no id".into()))?;
    // Verified first: burning an id on a forgery would let anyone block a real event.
    if !app.replay.first_use(&event_id, std::time::Instant::now()) {
        return Err(HiveError::Unauthenticated(
            "authorization already used".into(),
        ));
    }

    let hex = pubkey.to_hex();
    let actor = app.resolve(&hex).await?;
    if actor.level == Level::None
        && app
            .directory
            .relay_role(app.community, &hex)
            .await?
            .is_none()
        && actor.owner.is_none()
    {
        return Err(HiveError::PermissionDenied(
            "not a member of this community".into(),
        ));
    }
    let (token, expires_at) = app.minter.mint(&hex, app.community, actor.owner)?;
    Ok(Json(TokenReply { token, expires_at }))
}
