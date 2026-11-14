//! Read the environment once. A missing required value fails startup with the variable named.

pub struct Config {
    /// `BUZZ_HIVE_DATABASE_URL`
    pub database_url: String,
    /// `BUZZ_HIVE_LISTEN`, default `0.0.0.0:3100`
    pub listen: std::net::SocketAddr,
    /// `BUZZ_HIVE_PUBLIC_URL`, the origin clients reach, no trailing slash
    pub public_url: String,
    /// `BUZZ_HIVE_TOKEN_SECRET`, at least 32 bytes
    pub token_secret: String,
    /// `BUZZ_HIVE_COMMUNITY_HOST`, one community per instance
    pub community_host: String,
    /// `BUZZ_HIVE_ALLOWED_ORIGINS`, comma separated browser origins. Empty means none.
    pub allowed_origins: Vec<String>,
    /// `BUZZ_HIVE_WORKFLOWS_DIR`, a folder of workflow packages loaded beside the built-in
    /// ones. Unset means built-ins only.
    pub workflows_dir: Option<std::path::PathBuf>,
    /// `BUZZ_HIVE_NOSTR_KEY`, the secret key of the Beehive account, as `nsec1...` or 64
    /// hex characters. It signs the posts in the update channel. Unset means the service
    /// runs but cannot start a run.
    pub nostr_key: Option<String>,
    /// `BUZZ_HIVE_RELAY_URL`, the relay's WebSocket address, such as `ws://relay:3000`.
    /// Required with `BUZZ_HIVE_NOSTR_KEY`.
    pub relay_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let required = |name: &str| {
            get(name)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{name} is required"))
        };
        let listen = match get("BUZZ_HIVE_LISTEN").filter(|v| !v.trim().is_empty()) {
            Some(value) => value
                .parse()
                .map_err(|e| format!("BUZZ_HIVE_LISTEN is not an address ({e})"))?,
            None => ([0, 0, 0, 0], 3100).into(),
        };
        let token_secret = required("BUZZ_HIVE_TOKEN_SECRET")?;
        if token_secret.len() < 32 {
            return Err("BUZZ_HIVE_TOKEN_SECRET must be at least 32 bytes".into());
        }
        let allowed_origins = parse_origins(get("BUZZ_HIVE_ALLOWED_ORIGINS").as_deref())?;
        let optional = |name: &str| {
            get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let nostr_key = optional("BUZZ_HIVE_NOSTR_KEY");
        let relay_url = optional("BUZZ_HIVE_RELAY_URL");
        if nostr_key.is_some() && relay_url.is_none() {
            return Err("BUZZ_HIVE_RELAY_URL is required with BUZZ_HIVE_NOSTR_KEY".into());
        }
        if let Some(url) = &relay_url {
            if !(url.starts_with("ws://") || url.starts_with("wss://")) {
                return Err("BUZZ_HIVE_RELAY_URL must start with ws:// or wss://".into());
            }
        }
        Ok(Self {
            database_url: required("BUZZ_HIVE_DATABASE_URL")?,
            listen,
            public_url: required("BUZZ_HIVE_PUBLIC_URL")?
                .trim_end_matches('/')
                .to_owned(),
            token_secret,
            community_host: required("BUZZ_HIVE_COMMUNITY_HOST")?,
            allowed_origins,
            workflows_dir: get("BUZZ_HIVE_WORKFLOWS_DIR")
                .filter(|v| !v.trim().is_empty())
                .map(Into::into),
            nostr_key,
            relay_url,
        })
    }
}

/// Split a comma separated origin list. An entry must look like `scheme://host[:port]`,
/// because a typo here would silently turn browser access off.
fn parse_origins(raw: Option<&str>) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    for entry in raw.unwrap_or_default().split(',').map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        let valid = entry.split_once("://").is_some_and(|(scheme, host)| {
            !scheme.is_empty() && !host.is_empty() && !host.contains('/')
        }) && entry.parse::<axum::http::HeaderValue>().is_ok();
        if !valid {
            return Err(format!(
                "BUZZ_HIVE_ALLOWED_ORIGINS entry {entry:?} is not an origin like https://app.example.org"
            ));
        }
        origins.push(entry.to_owned());
    }
    Ok(origins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    const FULL: [(&str, &str); 4] = [
        ("BUZZ_HIVE_DATABASE_URL", "postgres://u:p@h/d"),
        ("BUZZ_HIVE_PUBLIC_URL", "http://localhost:3100/"),
        ("BUZZ_HIVE_TOKEN_SECRET", "0123456789abcdef0123456789abcdef"),
        ("BUZZ_HIVE_COMMUNITY_HOST", "localhost"),
    ];

    #[test]
    fn reads_every_value_and_defaults_the_listen_address() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!(config.listen.to_string(), "0.0.0.0:3100");
        assert_eq!(config.public_url, "http://localhost:3100");
        assert_eq!(config.community_host, "localhost");
    }

    #[test]
    fn a_missing_value_names_the_variable() {
        for (name, _) in FULL {
            let pairs: Vec<_> = FULL.iter().copied().filter(|(k, _)| *k != name).collect();
            let error = Config::from_lookup(env(&pairs)).err().unwrap();
            assert!(error.contains(name), "{error}");
        }
    }

    #[test]
    fn a_short_secret_is_refused() {
        let mut pairs = FULL.to_vec();
        pairs[2].1 = "short";
        assert!(Config::from_lookup(env(&pairs)).is_err());
    }

    #[test]
    fn origins_default_to_none_and_split_on_commas() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert!(config.allowed_origins.is_empty());
        let mut pairs = FULL.to_vec();
        pairs.push((
            "BUZZ_HIVE_ALLOWED_ORIGINS",
            " https://app.example.org, http://localhost:1420 ,",
        ));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(
            config.allowed_origins,
            ["https://app.example.org", "http://localhost:1420"]
        );
    }

    #[test]
    fn a_malformed_origin_is_refused() {
        for bad in [
            "*",
            "app.example.org",
            "https://app.example.org/path",
            "https://",
        ] {
            let mut pairs = FULL.to_vec();
            pairs.push(("BUZZ_HIVE_ALLOWED_ORIGINS", bad));
            let error = Config::from_lookup(env(&pairs)).err().unwrap();
            assert!(
                error.contains("BUZZ_HIVE_ALLOWED_ORIGINS"),
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn the_workflows_folder_is_optional() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!(config.workflows_dir, None);
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_WORKFLOWS_DIR", "/srv/workflows"));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.workflows_dir, Some("/srv/workflows".into()));
    }

    #[test]
    fn the_relay_account_is_optional_but_needs_its_relay() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!((config.nostr_key, config.relay_url), (None, None));

        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_NOSTR_KEY", "ab".repeat(32).leak()));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("BUZZ_HIVE_RELAY_URL"), "{error}");

        pairs.push(("BUZZ_HIVE_RELAY_URL", "http://relay:3000"));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("ws://"), "{error}");

        pairs.pop();
        pairs.push(("BUZZ_HIVE_RELAY_URL", "ws://relay:3000"));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.relay_url.as_deref(), Some("ws://relay:3000"));
        assert!(config.nostr_key.is_some());
    }

    #[test]
    fn a_bad_listen_address_is_refused() {
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_LISTEN", "not-an-address"));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("BUZZ_HIVE_LISTEN"));
    }
}
