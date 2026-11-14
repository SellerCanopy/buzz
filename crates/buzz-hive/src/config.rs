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
        Ok(Self {
            database_url: required("BUZZ_HIVE_DATABASE_URL")?,
            listen,
            public_url: required("BUZZ_HIVE_PUBLIC_URL")?
                .trim_end_matches('/')
                .to_owned(),
            token_secret,
            community_host: required("BUZZ_HIVE_COMMUNITY_HOST")?,
        })
    }
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
    fn a_bad_listen_address_is_refused() {
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_LISTEN", "not-an-address"));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("BUZZ_HIVE_LISTEN"));
    }
}
