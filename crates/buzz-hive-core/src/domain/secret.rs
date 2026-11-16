//! The names and values of agent and team secrets: what may be stored, and the names that
//! travel to the host supervisor.
//!
//! A value is held only in the secret store, written by the engine and read by the supervisor.
//! Everything here that is saved, logged or sent is a name.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The longest variable name.
pub const MAX_VAR_CHARS: usize = 64;
/// The longest value, as SSM's standard parameters allow.
pub const MAX_VALUE_BYTES: usize = 4096;
/// The most secrets one agent may be given, its own and granted together.
pub const MAX_SECRETS_PER_AGENT: usize = 64;

/// A secret value. It prints as `[redacted]` in `Debug` and `Display`, and has no
/// serializer, so a log line, an error or a response cannot carry it by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that writes it to the store.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Variables every container already has, which a secret may not shadow.
const AMBIENT: &[&str] = &["HOME", "PATH", "PWD", "USER", "SHELL"];
/// The deploy writes variables with these prefixes itself (`scripts/aws/agent_spec.py`), so a
/// secret may not shadow them. `BUZZ_` covers `BUZZ_PRIVATE_KEY` and every `BUZZ_ACP_` setting.
const RESERVED_PREFIXES: &[&str] = &["BUZZ_", "CLAUDE", "ANTHROPIC_", "OPENAI_", "CODEX_"];

/// Whether `var` may be stored as a secret: capital letters, digits and `_`, starting with a
/// letter, at most 64 characters, and not one the deploy or the container sets itself.
pub fn check_var(var: &str) -> Result<(), String> {
    let mut chars = var.chars();
    let shape = var.len() <= MAX_VAR_CHARS
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !shape {
        return Err(format!(
            "secret name {var:?} must be capital letters, digits and '_', starting with a letter, at most {MAX_VAR_CHARS} characters"
        ));
    }
    if AMBIENT.contains(&var) || RESERVED_PREFIXES.iter().any(|p| var.starts_with(p)) {
        return Err(format!(
            "secret name {var} is reserved: the deploy sets variables starting with {} and the container sets {}",
            RESERVED_PREFIXES.join(", "),
            AMBIENT.join(", ")
        ));
    }
    Ok(())
}

/// Whether `value` may be stored. The set of characters is the one `make agents-env` accepts,
/// because the value lands unquoted in an env file that Compose reads: no space, quote, `$` or
/// line break. The refusal never repeats the value.
pub fn check_value(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("a secret value must not be empty".into());
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(format!(
            "a secret value may be at most {MAX_VALUE_BYTES} bytes"
        ));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || ".:_~+/=@-".contains(c);
    if !value.chars().all(allowed) {
        return Err(
            "a secret value may hold only letters, digits and . _ ~ + / = : @ - (the value is not shown)"
                .into(),
        );
    }
    Ok(())
}

/// The secrets an agent is given, by variable name. This is all the supervisor is told: it
/// reads each value from the secret store itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretNames {
    /// Set for this agent alone.
    #[serde(default)]
    pub own: Vec<String>,
    /// Team secrets granted to this agent. An own secret of the same name wins.
    #[serde(default)]
    pub team: Vec<String>,
}

impl SecretNames {
    /// Checks every name and the total, for a list a caller sent whole.
    pub fn check(&self) -> Result<(), String> {
        if self.own.len() + self.team.len() > MAX_SECRETS_PER_AGENT {
            return Err(format!(
                "an agent may have at most {MAX_SECRETS_PER_AGENT} secrets"
            ));
        }
        for var in self.own.iter().chain(&self.team) {
            check_var(var)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_an_upper_case_variable_the_deploy_does_not_own() {
        for ok in ["GH_TOKEN", "DOCS_TOKEN", "A", "X1_Y2"] {
            assert_eq!(check_var(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "gh_token",
            "1TOKEN",
            "_TOKEN",
            "GH-TOKEN",
            "GH TOKEN",
            &"A".repeat(65),
            "BUZZ_PRIVATE_KEY",
            "BUZZ_ACP_SESSION_POLICY",
            "BUZZ_ANYTHING",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "PATH",
            "HOME",
        ] {
            assert!(check_var(bad).is_err(), "{bad:?}");
        }
        assert_eq!(check_var(&"A".repeat(64)), Ok(()));
    }

    #[test]
    fn a_value_is_bounded_and_the_refusal_never_repeats_it() {
        assert_eq!(check_value("ghp_abc.DEF-123/+=:@~"), Ok(()));
        assert_eq!(check_value(&"a".repeat(MAX_VALUE_BYTES)), Ok(()));
        for bad in [
            "",
            "has space",
            "new\nline",
            "quote'd",
            "dollar$VAR",
            "nul\0",
            &"a".repeat(MAX_VALUE_BYTES + 1),
        ] {
            let error = check_value(bad).unwrap_err();
            assert!(bad.is_empty() || !error.contains(bad), "{error}");
        }
    }

    #[test]
    fn a_list_of_names_is_checked_whole() {
        let names = |own: &[&str], team: &[&str]| SecretNames {
            own: own.iter().map(|s| (*s).to_owned()).collect(),
            team: team.iter().map(|s| (*s).to_owned()).collect(),
        };
        assert_eq!(names(&["A"], &["B"]).check(), Ok(()));
        assert!(names(&["BUZZ_PRIVATE_KEY"], &[]).check().is_err());
        assert!(names(&[], &["lower"]).check().is_err());
        let many: Vec<String> = (0..65).map(|i| format!("V{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(names(&many, &[]).check().is_err());
    }
}
