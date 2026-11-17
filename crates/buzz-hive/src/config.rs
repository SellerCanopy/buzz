//! Read the environment once. A missing required value fails startup with the variable named.

/// Where secret values are written. The host supervisor reads them from the same place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretsBackend {
    /// No store: setting a secret is refused and says how to configure one.
    None,
    /// Files under a folder, mode 0600, for local development and tests.
    Dir(std::path::PathBuf),
    /// SSM Parameter Store, written with the `aws` program at this path.
    Ssm { cli: std::path::PathBuf },
}

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
    /// `BUZZ_HIVE_WEB_DIR`, a built Beehive web page served at `/beehive/` for the phone's web
    /// view. Unset means the service serves no page.
    pub web_dir: Option<std::path::PathBuf>,
    /// `BUZZ_HIVE_NOSTR_KEY`, the secret key of the Beehive account, as `nsec1...` or 64
    /// hex characters. It signs the posts in the update channel. Unset means the service
    /// runs but cannot start a run.
    pub nostr_key: Option<String>,
    /// `BUZZ_HIVE_RELAY_URL`, the relay's WebSocket address, such as `ws://relay:3000`.
    /// Required with `BUZZ_HIVE_NOSTR_KEY`.
    pub relay_url: Option<String>,
    /// `BUZZ_HIVE_DB_MAX_CONNECTIONS`, default 10. Connections for Beehive's own tables.
    pub db_max_connections: u32,
    /// `BUZZ_HIVE_RELAY_DB_MAX_CONNECTIONS`, default 5. Connections for the read-only
    /// lookups in the relay's tables. It is a separate pool so a lookup never waits behind
    /// the transactions that are waiting for it.
    pub relay_db_max_connections: u32,
    /// `BUZZ_HIVE_DB_ACQUIRE_TIMEOUT_SECS`, default 10. How long a call waits for a free
    /// database connection.
    pub db_acquire_timeout: std::time::Duration,
    /// `GH_TOKEN`, the team's GitHub token, read from this process's own environment. It is
    /// not one of the agent secrets of `BUZZ_HIVE_SECRETS_*`: those fill agents' environments
    /// through the host supervisor, and this is Beehive's own. Unset means intake and the
    /// issue pickers are off.
    pub github_token: Option<String>,
    /// `BUZZ_HIVE_GITHUB_API_URL`, default `https://api.github.com`. A GitHub Enterprise
    /// server's API address, or a test server's.
    pub github_api_url: String,
    /// `BUZZ_HIVE_SUPERVISOR_SOCKET`, the Unix socket of the host supervisor that writes the
    /// agents' tool servers. Unset means a change is saved and reported as not applied.
    pub supervisor_socket: Option<std::path::PathBuf>,
    /// `BUZZ_HIVE_AGENTS_DIR`, the folder holding one folder per agent (`deploy/aws/agents`),
    /// read at start to seed the agents' tool servers. Unset means no agent is known.
    pub agents_dir: Option<std::path::PathBuf>,
    /// `BUZZ_HIVE_SECRETS_DIR` (files, local development) or `BUZZ_HIVE_SECRETS_BACKEND=ssm`
    /// (SSM, with `BUZZ_HIVE_AWS_CLI`, default `aws`). Not both. Neither means a secret
    /// cannot be set. Beehive only writes: it has no setting that reads a value back.
    pub secrets: SecretsBackend,
}

/// Longest any one statement may run, and the longest a statement waits for a lock.
pub const DB_STATEMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub const DB_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
        let count = |name: &str, default: u64, max: u64| -> Result<u64, String> {
            let Some(value) = optional(name) else {
                return Ok(default);
            };
            match value.parse::<u64>() {
                Ok(n) if (1..=max).contains(&n) => Ok(n),
                _ => Err(format!("{name} must be a whole number from 1 to {max}")),
            }
        };
        let secrets = match (
            optional("BUZZ_HIVE_SECRETS_DIR"),
            optional("BUZZ_HIVE_SECRETS_BACKEND"),
        ) {
            (None, None) => SecretsBackend::None,
            (Some(dir), None) => SecretsBackend::Dir(dir.into()),
            (None, Some(kind)) if kind == "ssm" => SecretsBackend::Ssm {
                cli: optional("BUZZ_HIVE_AWS_CLI")
                    .unwrap_or_else(|| "aws".into())
                    .into(),
            },
            (None, Some(_)) => return Err("BUZZ_HIVE_SECRETS_BACKEND must be ssm".into()),
            (Some(_), Some(_)) => {
                return Err(
                    "set BUZZ_HIVE_SECRETS_DIR or BUZZ_HIVE_SECRETS_BACKEND, not both".into(),
                )
            }
        };
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
            web_dir: optional("BUZZ_HIVE_WEB_DIR").map(Into::into),
            nostr_key,
            relay_url,
            db_max_connections: count("BUZZ_HIVE_DB_MAX_CONNECTIONS", 10, 200)? as u32,
            relay_db_max_connections: count("BUZZ_HIVE_RELAY_DB_MAX_CONNECTIONS", 5, 200)? as u32,
            db_acquire_timeout: std::time::Duration::from_secs(count(
                "BUZZ_HIVE_DB_ACQUIRE_TIMEOUT_SECS",
                10,
                300,
            )?),
            supervisor_socket: optional("BUZZ_HIVE_SUPERVISOR_SOCKET").map(Into::into),
            agents_dir: optional("BUZZ_HIVE_AGENTS_DIR").map(Into::into),
            secrets,
            github_token: optional("GH_TOKEN"),
            github_api_url: match optional("BUZZ_HIVE_GITHUB_API_URL") {
                Some(url) if url.starts_with("http://") || url.starts_with("https://") => url,
                Some(_) => {
                    return Err(
                        "BUZZ_HIVE_GITHUB_API_URL must start with http:// or https://".into(),
                    )
                }
                None => crate::github_adapter::DEFAULT_API_URL.to_owned(),
            },
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
    fn the_secret_store_is_files_or_ssm_and_never_both() {
        let with = |extra: &[(&str, &str)]| {
            let mut pairs = FULL.to_vec();
            pairs.extend_from_slice(extra);
            Config::from_lookup(env(&pairs))
        };
        assert_eq!(with(&[]).unwrap().secrets, SecretsBackend::None);
        assert_eq!(
            with(&[("BUZZ_HIVE_SECRETS_DIR", "/srv/secrets")])
                .unwrap()
                .secrets,
            SecretsBackend::Dir("/srv/secrets".into())
        );
        assert_eq!(
            with(&[("BUZZ_HIVE_SECRETS_BACKEND", "ssm")])
                .unwrap()
                .secrets,
            SecretsBackend::Ssm { cli: "aws".into() }
        );
        assert_eq!(
            with(&[
                ("BUZZ_HIVE_SECRETS_BACKEND", "ssm"),
                ("BUZZ_HIVE_AWS_CLI", "/usr/local/bin/aws")
            ])
            .unwrap()
            .secrets,
            SecretsBackend::Ssm {
                cli: "/usr/local/bin/aws".into()
            }
        );
        assert!(with(&[("BUZZ_HIVE_SECRETS_BACKEND", "vault")]).is_err());
        assert!(with(&[
            ("BUZZ_HIVE_SECRETS_BACKEND", "ssm"),
            ("BUZZ_HIVE_SECRETS_DIR", "/x")
        ])
        .is_err());
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
    fn the_web_folder_is_optional() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!(config.web_dir, None);
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_WEB_DIR", " /opt/buzz/beehive-web "));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.web_dir, Some("/opt/buzz/beehive-web".into()));
        pairs.push(("BUZZ_HIVE_WEB_DIR", "  "));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.web_dir, None);
    }

    #[test]
    fn the_pools_have_defaults_and_can_be_sized() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!(config.db_max_connections, 10);
        assert_eq!(config.relay_db_max_connections, 5);
        assert_eq!(config.db_acquire_timeout.as_secs(), 10);

        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_DB_MAX_CONNECTIONS", "20"));
        pairs.push(("BUZZ_HIVE_RELAY_DB_MAX_CONNECTIONS", "8"));
        pairs.push(("BUZZ_HIVE_DB_ACQUIRE_TIMEOUT_SECS", "3"));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.db_max_connections, 20);
        assert_eq!(config.relay_db_max_connections, 8);
        assert_eq!(config.db_acquire_timeout.as_secs(), 3);
    }

    #[test]
    fn a_bad_pool_size_names_the_variable() {
        for (name, bad) in [
            ("BUZZ_HIVE_DB_MAX_CONNECTIONS", "0"),
            ("BUZZ_HIVE_RELAY_DB_MAX_CONNECTIONS", "many"),
            ("BUZZ_HIVE_DB_ACQUIRE_TIMEOUT_SECS", "-1"),
        ] {
            let mut pairs = FULL.to_vec();
            pairs.push((name, bad));
            let error = Config::from_lookup(env(&pairs)).err().unwrap();
            assert!(error.contains(name), "{name}: {error}");
        }
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
    fn the_github_token_is_optional_and_the_address_defaults_to_github() {
        let config = Config::from_lookup(env(&FULL)).unwrap();
        assert_eq!(config.github_token, None);
        assert_eq!(config.github_api_url, "https://api.github.com");

        let mut pairs = FULL.to_vec();
        pairs.push(("GH_TOKEN", " example-token "));
        pairs.push(("BUZZ_HIVE_GITHUB_API_URL", "http://127.0.0.1:9/api/v3"));
        let config = Config::from_lookup(env(&pairs)).unwrap();
        assert_eq!(config.github_token.as_deref(), Some("example-token"));
        assert_eq!(config.github_api_url, "http://127.0.0.1:9/api/v3");
    }

    #[test]
    fn a_github_address_that_is_not_a_url_names_the_variable() {
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_GITHUB_API_URL", "api.example.org"));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("BUZZ_HIVE_GITHUB_API_URL"), "{error}");
    }

    #[test]
    fn a_bad_listen_address_is_refused() {
        let mut pairs = FULL.to_vec();
        pairs.push(("BUZZ_HIVE_LISTEN", "not-an-address"));
        let error = Config::from_lookup(env(&pairs)).err().unwrap();
        assert!(error.contains("BUZZ_HIVE_LISTEN"));
    }
}
