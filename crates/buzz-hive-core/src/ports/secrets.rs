//! Where secret values are written. The engine can put a value and can never read one back:
//! the host supervisor reads them (see `buzz-hive-supervisor`), and no type the engine holds
//! can.

use std::fmt;

/// Who a secret belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretScope {
    /// One agent's own secret, in its environment.
    Agent(String),
    /// A team secret, given to the agents granted it.
    Team,
}

impl SecretScope {
    /// Where the value lives, relative to the store's root: `agents/<name>/env/<VAR>` or
    /// `team/env/<VAR>`. SSM keeps it under `/buzz/`, and the local file store under
    /// `BUZZ_HIVE_SECRETS_DIR`, so both sides name one value the same way.
    pub fn path(&self, var: &str) -> String {
        match self {
            Self::Agent(agent) => format!("agents/{agent}/env/{var}"),
            Self::Team => format!("team/env/{var}"),
        }
    }
}

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

/// Why a value was not stored. The text never holds the value.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The store could not be reached or refused the write. Worth trying again.
    #[error("the secret store is unavailable: {0}")]
    Unavailable(String),
}

#[async_trait::async_trait]
pub trait SecretSink: Send + Sync {
    /// Put only. There is no read method on purpose. A value that exists is replaced.
    async fn put(
        &self,
        scope: SecretScope,
        var: &str,
        value: SecretString,
    ) -> Result<(), SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_prints_as_redacted_everywhere_it_can_be_formatted() {
        let secret = SecretString::new("ghp_known-value");
        for text in [
            format!("{secret}"),
            format!("{secret:?}"),
            format!("{secret:#?}"),
            format!("{:?}", Some(&secret)),
            format!("{:?}", vec![secret.clone()]),
        ] {
            assert!(text.contains("[redacted]"), "{text}");
            assert!(!text.contains("known-value"), "{text}");
        }
        assert_eq!(secret.expose(), "ghp_known-value");
    }

    #[test]
    fn a_value_is_named_by_its_scope_and_variable() {
        assert_eq!(SecretScope::Team.path("GH_TOKEN"), "team/env/GH_TOKEN");
        assert_eq!(
            SecretScope::Agent("coder".into()).path("DOCS_TOKEN"),
            "agents/coder/env/DOCS_TOKEN"
        );
    }
}
