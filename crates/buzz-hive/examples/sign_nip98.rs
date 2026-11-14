//! Print a `Nostr <base64>` Authorization value for a NIP-98 POST.
//!
//! `BUZZ_TEST_NSEC` holds a hex secret key (a bech32 nsec also works). The URL is
//! the first argument, default `http://localhost:3100/auth/token`.
//!
//! ```text
//! BUZZ_TEST_NSEC=<hex> cargo run -p buzz-hive --example sign_nip98 -- http://localhost:3100/auth/token
//! ```

use base64::Engine as _;
use nostr::nips::nip98::{HttpData, HttpMethod};
use nostr::{EventBuilder, JsonUtil, Keys, Url};

fn main() {
    let secret = std::env::var("BUZZ_TEST_NSEC").expect("set BUZZ_TEST_NSEC to a hex secret key");
    let keys = Keys::parse(secret.trim()).expect("BUZZ_TEST_NSEC is not a valid key");
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:3100/auth/token".into());
    let url = Url::parse(&url).expect("the URL is not valid");
    let event = EventBuilder::http_auth(HttpData::new(url, HttpMethod::POST))
        .sign_with_keys(&keys)
        .expect("sign the event");
    let encoded = base64::engine::general_purpose::STANDARD.encode(event.as_json());
    println!("Nostr {encoded}");
}
