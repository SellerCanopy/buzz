//! The supervisor's settings, read once from the environment.

use std::path::PathBuf;

/// How an agent is restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// `docker compose` recreates the agent's container from `compose_file`, which makes it read
    /// its env file and `mcp.json` again. A plain `compose restart` would not.
    Compose { compose_file: PathBuf },
    /// A program run with the agent's name as its only argument.
    Command(PathBuf),
}

/// Where the supervisor reads secret values from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Secrets {
    /// No store: an apply that names a secret fails.
    None,
    /// Files under a folder, for local development.
    Dir(PathBuf),
    /// SSM Parameter Store, read with the `aws` program at this path.
    Ssm { cli: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `BUZZ_HIVE_SUPERVISOR_SOCKET`
    pub socket: PathBuf,
    /// `BUZZ_HIVE_SUPERVISOR_PUBKEY`, the engine's signing key as 64 hex characters. The engine
    /// logs it at start.
    pub engine_key: String,
    /// `BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY`, the Beehive account's public key as 64 hex
    /// characters, which every agent must answer.
    pub hive_pubkey: String,
    /// `BUZZ_HIVE_SUPERVISOR_AGENTS_SRC`, the `deploy/aws/agents` folder. Its subfolders are the
    /// only agents a call may name.
    pub agents_src: PathBuf,
    /// `BUZZ_HIVE_SUPERVISOR_RENDER_DIR`, where the deploy renders `<name>.env` and
    /// `<name>/config/mcp.json` (`/opt/buzz/agents`).
    pub render_dir: PathBuf,
    /// `BUZZ_HIVE_SUPERVISOR_RESTART_CMD`, a program that takes the agent name. Unset means
    /// the Compose recreate of `compose.agents.yml`, next to `agents_src`.
    pub restart: Restart,
    /// `BUZZ_HIVE_SUPERVISOR_SECRETS_DIR` (files, local development) or
    /// `BUZZ_HIVE_SUPERVISOR_SECRETS=ssm` (SSM, with `BUZZ_HIVE_SUPERVISOR_AWS_CLI`, default
    /// `aws`). Not both. Neither means no secret can be filled in.
    pub secrets: Secrets,
    /// `BUZZ_HIVE_SUPERVISOR_SOCKET_UID`, the user id that owns the socket, which is the one
    /// `buzz-hive` runs as. Unset leaves the owner as it is.
    pub socket_uid: Option<u32>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let optional = |name: &str| {
            get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let required = |name: &str| optional(name).ok_or_else(|| format!("{name} is required"));
        let key = |name: &str| -> Result<String, String> {
            let value = required(name)?;
            let ok = value.len() == 64 && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
            if ok {
                Ok(value)
            } else {
                Err(format!("{name} must be 64 lowercase hex characters"))
            }
        };
        let agents_src = PathBuf::from(required("BUZZ_HIVE_SUPERVISOR_AGENTS_SRC")?);
        let restart = match optional("BUZZ_HIVE_SUPERVISOR_RESTART_CMD") {
            Some(cmd) => Restart::Command(cmd.into()),
            None => Restart::Compose {
                compose_file: agents_src
                    .parent()
                    .ok_or("BUZZ_HIVE_SUPERVISOR_AGENTS_SRC has no parent folder")?
                    .join("compose.agents.yml"),
            },
        };
        let socket_uid = optional("BUZZ_HIVE_SUPERVISOR_SOCKET_UID")
            .map(|v| {
                v.parse::<u32>()
                    .map_err(|_| "BUZZ_HIVE_SUPERVISOR_SOCKET_UID must be a user id".to_owned())
            })
            .transpose()?;
        let secrets = match (
            optional("BUZZ_HIVE_SUPERVISOR_SECRETS_DIR"),
            optional("BUZZ_HIVE_SUPERVISOR_SECRETS"),
        ) {
            (None, None) => Secrets::None,
            (Some(dir), None) => Secrets::Dir(dir.into()),
            (None, Some(kind)) if kind == "ssm" => Secrets::Ssm {
                cli: optional("BUZZ_HIVE_SUPERVISOR_AWS_CLI")
                    .unwrap_or_else(|| "aws".into())
                    .into(),
            },
            (None, Some(_)) => return Err("BUZZ_HIVE_SUPERVISOR_SECRETS must be ssm".into()),
            (Some(_), Some(_)) => return Err(
                "set BUZZ_HIVE_SUPERVISOR_SECRETS_DIR or BUZZ_HIVE_SUPERVISOR_SECRETS, not both"
                    .into(),
            ),
        };
        Ok(Self {
            socket: required("BUZZ_HIVE_SUPERVISOR_SOCKET")?.into(),
            engine_key: key("BUZZ_HIVE_SUPERVISOR_PUBKEY")?,
            hive_pubkey: key("BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY")?,
            agents_src,
            render_dir: required("BUZZ_HIVE_SUPERVISOR_RENDER_DIR")?.into(),
            restart,
            secrets,
            socket_uid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(missing: &'static str, restart: Option<&'static str>) -> Result<Config, String> {
        let key = "ab".repeat(32);
        Config::from_lookup(|name| {
            if name == missing {
                return None;
            }
            match name {
                "BUZZ_HIVE_SUPERVISOR_SOCKET" => Some("/run/s.sock".into()),
                "BUZZ_HIVE_SUPERVISOR_PUBKEY" | "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY" => {
                    Some(key.clone())
                }
                "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC" => Some("/opt/buzz/deploy/aws/agents".into()),
                "BUZZ_HIVE_SUPERVISOR_RENDER_DIR" => Some("/opt/buzz/agents".into()),
                "BUZZ_HIVE_SUPERVISOR_RESTART_CMD" => restart.map(str::to_owned),
                _ => None,
            }
        })
    }

    #[test]
    fn the_restart_defaults_to_the_agents_compose_file() {
        assert_eq!(
            lookup("", None).unwrap().restart,
            Restart::Compose {
                compose_file: "/opt/buzz/deploy/aws/compose.agents.yml".into()
            }
        );
        assert_eq!(
            lookup("", Some("/usr/local/bin/stub")).unwrap().restart,
            Restart::Command("/usr/local/bin/stub".into())
        );
    }

    #[test]
    fn the_secret_store_is_files_or_ssm_and_never_both() {
        let with = |pairs: &'static [(&'static str, &'static str)]| {
            let key = "ab".repeat(32);
            Config::from_lookup(|name| {
                if let Some((_, v)) = pairs.iter().find(|(k, _)| *k == name) {
                    return Some((*v).to_owned());
                }
                match name {
                    "BUZZ_HIVE_SUPERVISOR_SOCKET" => Some("/run/s.sock".into()),
                    "BUZZ_HIVE_SUPERVISOR_PUBKEY" | "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY" => {
                        Some(key.clone())
                    }
                    "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC" => Some("/opt/buzz/deploy/aws/agents".into()),
                    "BUZZ_HIVE_SUPERVISOR_RENDER_DIR" => Some("/opt/buzz/agents".into()),
                    _ => None,
                }
            })
        };
        assert_eq!(with(&[]).unwrap().secrets, Secrets::None);
        assert_eq!(
            with(&[("BUZZ_HIVE_SUPERVISOR_SECRETS_DIR", "/srv/secrets")])
                .unwrap()
                .secrets,
            Secrets::Dir("/srv/secrets".into())
        );
        assert_eq!(
            with(&[("BUZZ_HIVE_SUPERVISOR_SECRETS", "ssm")])
                .unwrap()
                .secrets,
            Secrets::Ssm { cli: "aws".into() }
        );
        assert_eq!(
            with(&[
                ("BUZZ_HIVE_SUPERVISOR_SECRETS", "ssm"),
                ("BUZZ_HIVE_SUPERVISOR_AWS_CLI", "/usr/local/bin/aws")
            ])
            .unwrap()
            .secrets,
            Secrets::Ssm {
                cli: "/usr/local/bin/aws".into()
            }
        );
        assert!(with(&[("BUZZ_HIVE_SUPERVISOR_SECRETS", "vault")]).is_err());
        assert!(with(&[
            ("BUZZ_HIVE_SUPERVISOR_SECRETS", "ssm"),
            ("BUZZ_HIVE_SUPERVISOR_SECRETS_DIR", "/x")
        ])
        .is_err());
    }

    #[test]
    fn every_setting_but_the_restart_is_required() {
        for name in [
            "BUZZ_HIVE_SUPERVISOR_SOCKET",
            "BUZZ_HIVE_SUPERVISOR_PUBKEY",
            "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY",
            "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC",
            "BUZZ_HIVE_SUPERVISOR_RENDER_DIR",
        ] {
            assert!(lookup(name, None).unwrap_err().contains(name), "{name}");
        }
    }
}
