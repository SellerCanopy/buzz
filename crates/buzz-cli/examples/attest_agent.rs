//! Attest a cloud agent for its owner so Buzz Desktop lists it.
//!
//! Publishes the owner-signed managed-agent record (kind 30177) for the agent
//! and prints the NIP-OA auth tag the agent must put on its profile.
//!
//! Usage:
//!   BUZZ_OWNER_KEY=<hex or nsec> BUZZ_RELAY_URL=wss://relay.example.com \
//!     cargo run --release -p buzz-cli --example attest_agent -- \
//!     <agent_pubkey_hex> <name> <owner-only|allowlist|anyone> [allowlist_pubkeys_csv] [parallelism]
//!
//! The owner key is read from the environment so it never appears in a
//! process listing. If BUZZ_OWNER_PUBKEY is set, the key must belong to it.
//! Stdout carries only the auth tag JSON.

use buzz_sdk::nip_oa;
use buzz_ws_client::publish_event;
use nostr::{EventBuilder, Keys, Kind, PublicKey, Tag};

const KIND_MANAGED_AGENT: u16 = 30177;
const PUBLISH_TIMEOUT_SECS: u64 = 60;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if let Err(message) = run().await {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if !(4..=6).contains(&args.len()) {
        return Err(format!(
            "usage: {} <agent_pubkey_hex> <name> <owner-only|allowlist|anyone> [allowlist_pubkeys_csv] [parallelism]",
            args[0]
        ));
    }
    let agent_pubkey = PublicKey::from_hex(&args[1]).map_err(|e| format!("agent pubkey: {e}"))?;
    let name = &args[2];
    let respond_to = args[3].as_str();
    if !matches!(respond_to, "owner-only" | "allowlist" | "anyone") {
        return Err(format!(
            "respond_to must be owner-only, allowlist or anyone (got {respond_to})"
        ));
    }

    let parallelism: u32 = match args.get(5) {
        Some(value) => value
            .parse()
            .ok()
            .filter(|n| (1..=32).contains(n))
            .ok_or_else(|| {
                format!("parallelism must be a whole number from 1 to 32 (got {value})")
            })?,
        None => 1,
    };

    let owner_key = std::env::var("BUZZ_OWNER_KEY").map_err(|_| "BUZZ_OWNER_KEY is not set")?;
    let owner = Keys::parse(owner_key.trim()).map_err(|e| format!("BUZZ_OWNER_KEY: {e}"))?;
    if let Ok(expected) = std::env::var("BUZZ_OWNER_PUBKEY") {
        if owner.public_key().to_hex() != expected.trim().to_ascii_lowercase() {
            return Err(format!(
                "BUZZ_OWNER_KEY belongs to {}, not the relay owner {expected}",
                owner.public_key().to_hex()
            ));
        }
    }
    let relay_url = std::env::var("BUZZ_RELAY_URL").map_err(|_| "BUZZ_RELAY_URL is not set")?;

    // Same fields the desktop app publishes for an agent it creates.
    let mut content = serde_json::json!({
        "name": name,
        "parallelism": parallelism,
        "respond_to": respond_to,
    });
    if let Some(allowlist) = args.get(4).filter(|csv| !csv.is_empty()) {
        let keys: Vec<&str> = allowlist.split(',').collect();
        content["respond_to_allowlist"] = serde_json::json!(keys);
    }
    let content = content.to_string();
    let d_tag = Tag::parse(["d", agent_pubkey.to_hex().as_str()]).map_err(|e| e.to_string())?;
    let event = EventBuilder::new(Kind::Custom(KIND_MANAGED_AGENT), content)
        .tags([d_tag])
        .sign_with_keys(&owner)
        .map_err(|e| format!("signing failed: {e}"))?;

    let ok = publish_event(&relay_url, event, &owner, None, PUBLISH_TIMEOUT_SECS)
        .await
        .map_err(|e| format!("publish failed: {e}"))?;
    if !ok.accepted {
        return Err(format!("relay rejected the agent record: {}", ok.message));
    }
    eprintln!("published agent record for {name}");

    let tag = nip_oa::compute_auth_tag(&owner, &agent_pubkey, "").map_err(|e| e.to_string())?;
    println!("{tag}");
    Ok(())
}
