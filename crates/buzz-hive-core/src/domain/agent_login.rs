//! Signing an agent in to an OAuth tool server: what the admin pastes back, and the two states
//! a server can be in.
//!
//! The admin opens a link, signs in at the provider, and pastes the address the browser ended on
//! (`http://localhost:PORT/...?code=...`). That address can carry the authorization code, so it
//! is held in a type that prints as `[redacted]` and every refusal here is a fixed sentence
//! that does not repeat it. The same check runs where the admin pastes it and in the host
//! supervisor, which writes it to the waiting process.

use crate::domain::secret::SecretString;

/// The longest address accepted.
pub const MAX_CALLBACK_CHARS: usize = 2048;

/// The sentence every refusal of a pasted address uses. It never holds any of the address.
pub const NOT_A_CALLBACK: &str = "that is not the address the browser ended on: it must start with http://localhost: and a port, and carry a code (the address is not shown)";

/// A pasted sign-in address that passed [`CallbackUrl::parse`]. `Debug` prints `[redacted]`, and
/// there is no `Display` or serializer, so a log line or an error cannot carry it by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackUrl(SecretString);

impl CallbackUrl {
    /// The address, when it has the callback shape the CLI waits for: `http://localhost:PORT/`
    /// (or `127.0.0.1`), a path, and a query with a non-empty `code`. No user, fragment, space
    /// or control character, so what is written to the process is one line and nothing else.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        if is_callback(text) {
            Ok(Self(SecretString::new(text)))
        } else {
            Err(NOT_A_CALLBACK)
        }
    }

    /// The address, for the one place that writes it to the waiting process or the wire.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

fn is_callback(text: &str) -> bool {
    if text.is_empty()
        || text.len() > MAX_CALLBACK_CHARS
        || !text.chars().all(|c| c.is_ascii_graphic())
    {
        return false;
    }
    let Some(rest) = text.strip_prefix("http://") else {
        return false;
    };
    let Some((host, rest)) = rest.split_once(':') else {
        return false;
    };
    if host != "localhost" && host != "127.0.0.1" {
        return false;
    }
    let Some((port, rest)) = rest.split_once('/') else {
        return false;
    };
    if port.is_empty()
        || port.len() > 5
        || !port.chars().all(|c| c.is_ascii_digit())
        || port.parse::<u32>().map_or(true, |p| p == 0 || p > 65_535)
    {
        return false;
    }
    let Some((_path, query)) = rest.split_once('?') else {
        return false;
    };
    if query.contains('#') || query.contains('?') {
        return false;
    }
    query
        .split('&')
        .any(|pair| pair.strip_prefix("code=").is_some_and(|v| !v.is_empty()))
}

/// Where a tool server stands for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    NeedsSignIn,
    Connected,
}

impl LoginState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsSignIn => "needs-sign-in",
            Self::Connected => "connected",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "needs-sign-in" => Some(Self::NeedsSignIn),
            "connected" => Some(Self::Connected),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_callback_shape_is_a_localhost_address_with_a_port_and_a_code() {
        for ok in [
            "http://localhost:45219/callback?code=abc&state=xyz",
            "http://localhost:8080/oauth/callback?state=s&code=a.b-c_d",
            "http://127.0.0.1:3118/cb?code=1",
        ] {
            assert!(CallbackUrl::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "https://localhost:45219/callback?code=abc",
            "http://example.org:45219/callback?code=abc",
            "http://localhost.example.org:45219/callback?code=abc",
            "http://localhost/callback?code=abc",
            "http://localhost:/callback?code=abc",
            "http://localhost:0/callback?code=abc",
            "http://localhost:99999/callback?code=abc",
            "http://localhost:45219",
            "http://localhost:45219/callback",
            "http://localhost:45219/callback?state=xyz",
            "http://localhost:45219/callback?code=",
            "http://localhost:45219/callback?error=access_denied",
            "http://localhost:45219/callback?code=abc#frag",
            "http://localhost:80@evil.example.org/callback?code=abc",
            "http://user@localhost:45219/callback?code=abc",
            "http://localhost:45219/callback?code=abc def",
            "http://localhost:45219/callback?code=abc\nhttp://localhost:1/x?code=2",
            "http://localhost:45219/callback?code=abc\r",
            " http://localhost:45219/callback?code=abc",
            "HTTP://localhost:45219/callback?code=abc",
            "the code is abc",
        ] {
            assert_eq!(CallbackUrl::parse(bad), Err(NOT_A_CALLBACK), "{bad:?}");
        }
        let long = format!(
            "http://localhost:1234/cb?code={}",
            "a".repeat(MAX_CALLBACK_CHARS)
        );
        assert!(CallbackUrl::parse(&long).is_err());
    }

    #[test]
    fn a_refusal_and_a_debug_print_never_hold_the_address() {
        let text = "http://localhost:45219/callback?code=TOPSECRETCODE";
        let url = CallbackUrl::parse(text).unwrap();
        assert!(!format!("{url:?}").contains("TOPSECRETCODE"));
        let refused = CallbackUrl::parse("http://evil.example.org:1/x?code=TOPSECRETCODE");
        assert!(!format!("{refused:?}").contains("TOPSECRETCODE"));
        assert_eq!(url.expose(), text);
    }

    #[test]
    fn a_state_is_read_back_from_its_wire_form() {
        use LoginState::*;
        assert_eq!(LoginState::parse("connected"), Some(Connected));
        assert_eq!(LoginState::parse(Connected.as_str()), Some(Connected));
        assert_eq!(LoginState::parse(NeedsSignIn.as_str()), Some(NeedsSignIn));
        assert_eq!(LoginState::parse("signed-in"), None);
    }
}
