//! Where the supervisor reads secret values from. This is the only code that reads one: the
//! engine writes a value and never reads it back, and nothing in `buzz-hive-core` or the
//! engine's crates can name this trait.
//!
//! Two sources: the files of a folder for local development, and SSM Parameter Store through
//! the host's `aws` program for production. Both name a value the way the engine's sink does
//! (`SecretScope::path`).

use std::path::PathBuf;
use std::time::Duration;

use buzz_hive_core::ports::secrets::{SecretScope, SecretString};
use tokio::process::Command;

/// The longest a read of one parameter may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait SecretSource: Send + Sync {
    /// The value, or `None` when it was never set. An error says what failed and never holds
    /// a value.
    async fn read(&self, scope: &SecretScope, var: &str) -> Result<Option<SecretString>, String>;
}

/// No store is configured. Any read says so, which an apply that names no secret never makes.
pub struct NoSecrets;

#[async_trait::async_trait]
impl SecretSource for NoSecrets {
    async fn read(&self, _: &SecretScope, _: &str) -> Result<Option<SecretString>, String> {
        Err("the supervisor has no secret store: set BUZZ_HIVE_SUPERVISOR_SECRETS_DIR or BUZZ_HIVE_SUPERVISOR_SECRETS=ssm".into())
    }
}

/// Values in files under `dir`, as the engine's `FileSecretSink` writes them.
pub struct FileSecretSource {
    pub dir: PathBuf,
}

#[async_trait::async_trait]
impl SecretSource for FileSecretSource {
    async fn read(&self, scope: &SecretScope, var: &str) -> Result<Option<SecretString>, String> {
        let path = self.dir.join(scope.path(var));
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => Ok(Some(SecretString::new(text.trim_end()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        }
    }
}

/// SSM SecureString parameters at `/buzz/<path>`, read with the `aws` program the deploy
/// scripts already use, under the host's instance role. The supervisor has read access; the
/// engine does not.
pub struct SsmSecretSource {
    pub cli: PathBuf,
}

#[async_trait::async_trait]
impl SecretSource for SsmSecretSource {
    async fn read(&self, scope: &SecretScope, var: &str) -> Result<Option<SecretString>, String> {
        let name = format!("/buzz/{}", scope.path(var));
        let mut command = Command::new(&self.cli);
        command
            .args(["ssm", "get-parameter", "--with-decryption", "--name"])
            .arg(&name)
            .args(["--query", "Parameter.Value", "--output", "text"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(READ_TIMEOUT, command.output())
            .await
            .map_err(|_| format!("reading {name} took too long"))?
            .map_err(|e| format!("could not run {}: {e}", self.cli.display()))?;
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout);
            return Ok(Some(SecretString::new(value.trim_end())));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("ParameterNotFound") {
            return Ok(None);
        }
        let tail: String = stderr
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(format!(
            "reading {name} failed ({}): {}",
            output.status,
            tail.trim()
        ))
    }
}
