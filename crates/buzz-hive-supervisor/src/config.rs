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

/// How a program is run inside an agent's container, for the Claude CLI's sign-in and its
/// `mcp list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exec {
    /// `docker compose exec -T agent <argv>` in the agent's Compose project, as the container's
    /// own user, so the CLI keeps its tokens in that user's home.
    Compose { compose_file: PathBuf },
    /// A program run with the agent's name first and then the whole argv (`claude mcp login
    /// ...`), standing in for `docker compose exec`. Tests and the local stack point it at a
    /// stub.
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
    /// `BUZZ_HIVE_SUPERVISOR_EXEC_CMD`, a program that stands in for `docker compose exec` when
    /// the sign-in calls run the Claude CLI in an agent's container. It is run as
    /// `<program> <agent> claude mcp login <server> --no-browser` and
    /// `<program> <agent> claude mcp list`, with the sign-in's stdin and stdout piped through
    /// it. Unset means `docker compose exec -T agent ...` on the same Compose project the
    /// restart uses.
    pub exec: Exec,
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
        let compose_file = agents_src
            .parent()
            .ok_or("BUZZ_HIVE_SUPERVISOR_AGENTS_SRC has no parent folder")?
            .join("compose.agents.yml");
        let restart = match optional("BUZZ_HIVE_SUPERVISOR_RESTART_CMD") {
            Some(cmd) => Restart::Command(cmd.into()),
            None => Restart::Compose {
                compose_file: compose_file.clone(),
            },
        };
        let exec = match optional("BUZZ_HIVE_SUPERVISOR_EXEC_CMD") {
            Some(cmd) => Exec::Command(cmd.into()),
            None => Exec::Compose { compose_file },
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
            exec,
            secrets,
            socket_uid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(missing: &'static str, restart: Option<&'static str>) -> Result<Config, String> {
        lookup_exec(missing, restart, None)
    }

    fn lookup_exec(
        missing: &'static str,
        restart: Option<&'static str>,
        exec: Option<&'static str>,
    ) -> Result<Config, String> {
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
                "BUZZ_HIVE_SUPERVISOR_EXEC_CMD" => exec.map(str::to_owned),
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
    fn the_exec_defaults_to_compose_exec_and_a_program_stands_in_for_it() {
        assert_eq!(
            lookup("", None).unwrap().exec,
            Exec::Compose {
                compose_file: "/opt/buzz/deploy/aws/compose.agents.yml".into()
            }
        );
        assert_eq!(
            lookup_exec("", None, Some("/usr/local/bin/exec-stub"))
                .unwrap()
                .exec,
            Exec::Command("/usr/local/bin/exec-stub".into())
        );
        // The two settings are independent: a stub restart keeps the real exec.
        assert!(matches!(
            lookup("", Some("/x")).unwrap().exec,
            Exec::Compose { .. }
        ));
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

    fn with_values(pairs: &'static [(&'static str, &'static str)]) -> Result<Config, String> {
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
    }

    #[test]
    fn both_keys_must_be_64_lowercase_hex_characters_and_the_error_names_the_variable() {
        let good = "ab".repeat(32);
        assert_eq!(
            with_values(&[]).unwrap().engine_key,
            good,
            "a good key is kept as it is"
        );
        let bad: [&'static str; 5] = [
            // 63 and 65 characters.
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            // Uppercase hex.
            "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",
            // A 'g' in an otherwise right-sized key.
            "gbabababababababababababababababababababababababababababababababab",
            // An nsec is not the hex form the allowlist and the verifier take.
            "nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
        ];
        assert_eq!(bad[0].len(), 63);
        assert_eq!(bad[1].len(), 65);
        for variable in [
            "BUZZ_HIVE_SUPERVISOR_PUBKEY",
            "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY",
        ] {
            for value in bad {
                let key = "ab".repeat(32);
                let error = Config::from_lookup(|name| match name {
                    n if n == variable => Some(value.to_owned()),
                    "BUZZ_HIVE_SUPERVISOR_SOCKET" => Some("/run/s.sock".into()),
                    "BUZZ_HIVE_SUPERVISOR_PUBKEY" | "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY" => {
                        Some(key.clone())
                    }
                    "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC" => Some("/opt/buzz/deploy/aws/agents".into()),
                    "BUZZ_HIVE_SUPERVISOR_RENDER_DIR" => Some("/opt/buzz/agents".into()),
                    _ => None,
                })
                .unwrap_err();
                assert!(error.contains(variable), "{variable} {value}: {error}");
                assert!(error.contains("64 lowercase hex"), "{error}");
            }
        }
    }

    #[test]
    fn the_socket_owner_is_a_user_id_or_nothing() {
        assert_eq!(with_values(&[]).unwrap().socket_uid, None);
        assert_eq!(
            with_values(&[("BUZZ_HIVE_SUPERVISOR_SOCKET_UID", "1000")])
                .unwrap()
                .socket_uid,
            Some(1000)
        );
        // Padding around a value is not part of it.
        assert_eq!(
            with_values(&[("BUZZ_HIVE_SUPERVISOR_SOCKET_UID", " 7 ")])
                .unwrap()
                .socket_uid,
            Some(7)
        );
        for value in ["abc", "-1", "1.5", "4294967296"] {
            let pairs: &'static [(&str, &str)] = Box::leak(Box::new([(
                "BUZZ_HIVE_SUPERVISOR_SOCKET_UID",
                Box::leak(value.to_owned().into_boxed_str()) as &'static str,
            )]));
            let error = with_values(pairs).unwrap_err();
            assert!(
                error.contains("BUZZ_HIVE_SUPERVISOR_SOCKET_UID"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn a_blank_value_is_a_missing_one_and_a_root_agents_folder_is_refused() {
        for name in [
            "BUZZ_HIVE_SUPERVISOR_SOCKET",
            "BUZZ_HIVE_SUPERVISOR_RENDER_DIR",
            "BUZZ_HIVE_SUPERVISOR_PUBKEY",
        ] {
            let pairs: &'static [(&str, &str)] = Box::leak(Box::new([(name_static(name), "   ")]));
            let error = with_values(pairs).unwrap_err();
            assert!(
                error.contains(name) && error.contains("is required"),
                "{name}: {error}"
            );
        }
        // A blank restart program is the default restart, not a program named "".
        assert!(matches!(
            with_values(&[("BUZZ_HIVE_SUPERVISOR_RESTART_CMD", "  ")])
                .unwrap()
                .restart,
            Restart::Compose { .. }
        ));
        let error = with_values(&[("BUZZ_HIVE_SUPERVISOR_AGENTS_SRC", "/")]).unwrap_err();
        assert!(error.contains("no parent folder"), "{error}");
    }

    fn name_static(name: &str) -> &'static str {
        Box::leak(name.to_owned().into_boxed_str())
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
