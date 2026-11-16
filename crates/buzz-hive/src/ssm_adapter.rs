//! The production secret store: SSM Parameter Store SecureString parameters at
//! `/buzz/<path>`, written through the `aws` program under the host's instance role. That
//! role may put and may not get; the supervisor's may get. Nothing here reads a value.
//!
//! The value reaches `aws` through a mode 0600 file (`--value file://...`), never on the
//! command line, where `ps` would show it. The file is removed whether or not the call
//! worked.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::Duration;

use buzz_hive_core::ports::secrets::{SecretError, SecretScope, SecretSink, SecretString};
use tokio::process::Command;

/// The longest a write of one parameter may take.
const PUT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SsmSecretSink {
    /// The `aws` program. A path, so a test can name a stand-in.
    cli: PathBuf,
}

impl SsmSecretSink {
    pub fn new(cli: PathBuf) -> Self {
        Self { cli }
    }
}

/// The 0600 file the value is handed over in, removed when dropped.
struct ValueFile(PathBuf);

impl ValueFile {
    fn create(value: &SecretString) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "buzz-hive-secret-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        // Owned before the write, so a failed write still removes the file.
        let owned = Self(path);
        file.write_all(value.expose().as_bytes())?;
        Ok(owned)
    }
}

impl Drop for ValueFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[async_trait::async_trait]
impl SecretSink for SsmSecretSink {
    async fn put(
        &self,
        scope: SecretScope,
        var: &str,
        value: SecretString,
    ) -> Result<(), SecretError> {
        let name = format!("/buzz/{}", scope.path(var));
        let file = ValueFile::create(&value)
            .map_err(|e| SecretError::Unavailable(format!("could not stage {name}: {e}")))?;
        let mut command = Command::new(&self.cli);
        command
            .args([
                "ssm",
                "put-parameter",
                "--type",
                "SecureString",
                "--overwrite",
                "--name",
            ])
            .arg(&name)
            .arg("--value")
            .arg(format!("file://{}", file.0.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(PUT_TIMEOUT, command.output())
            .await
            .map_err(|_| SecretError::Unavailable(format!("writing {name} took too long")))?
            .map_err(|e| {
                SecretError::Unavailable(format!("could not run {}: {e}", self.cli.display()))
            })?;
        if output.status.success() {
            return Ok(());
        }
        // The program's complaint, which names a parameter and a policy and holds no value.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(SecretError::Unavailable(format!(
            "writing {name} failed ({}): {}",
            output.status,
            tail.trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for `aws` that records its arguments and what the value file held, and
    /// fails when asked to. It never reaches a network.
    fn stub(dir: &std::path::Path, fail: bool) -> PathBuf {
        let program = dir.join("aws");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n\
                 echo \"$@\" >> '{args}'\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = --value ] && v=$2; shift; done\n\
                 f=${{v#file://}}\n\
                 stat -f %Lp \"$f\" > '{mode}' 2>/dev/null || stat -c %a \"$f\" > '{mode}'\n\
                 cat \"$f\" > '{seen}'\n\
                 {tail}",
                args = dir.join("args.log").display(),
                mode = dir.join("mode.txt").display(),
                seen = dir.join("value.txt").display(),
                tail = if fail {
                    "echo 'An error occurred (AccessDeniedException) when calling PutParameter' >&2\nexit 254\n"
                } else {
                    "exit 0\n"
                },
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    #[tokio::test]
    async fn a_value_goes_to_aws_in_a_private_file_and_never_on_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let sink = SsmSecretSink::new(stub(dir.path(), false));
        sink.put(
            SecretScope::Agent("coder".into()),
            "DOCS_TOKEN",
            SecretString::new("docs.Value-123"),
        )
        .await
        .unwrap();
        let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        assert!(
            args.starts_with(
                "ssm put-parameter --type SecureString --overwrite --name /buzz/agents/coder/env/DOCS_TOKEN --value file://"
            ),
            "{args}"
        );
        assert!(!args.contains("docs.Value-123"), "{args}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("value.txt")).unwrap(),
            "docs.Value-123"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mode.txt"))
                .unwrap()
                .trim(),
            "600"
        );
        // The staged file is gone.
        let path = args.split("file://").nth(1).unwrap().trim();
        assert!(!std::path::Path::new(path).exists());
    }

    #[tokio::test]
    async fn a_refusal_comes_back_without_the_value_and_the_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let sink = SsmSecretSink::new(stub(dir.path(), true));
        let error = sink
            .put(
                SecretScope::Team,
                "GH_TOKEN",
                SecretString::new("ghp_not-shown"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("AccessDeniedException"), "{error}");
        assert!(error.contains("/buzz/team/env/GH_TOKEN"), "{error}");
        assert!(!error.contains("ghp_not-shown"), "{error}");
        let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        let path = args.split("file://").nth(1).unwrap().trim();
        assert!(!std::path::Path::new(path).exists());
    }

    #[tokio::test]
    async fn a_program_that_is_not_there_is_unavailable() {
        let sink = SsmSecretSink::new("/nonexistent/aws".into());
        let error = sink
            .put(
                SecretScope::Team,
                "GH_TOKEN",
                SecretString::new("ghp_not-shown"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("/nonexistent/aws") && !error.contains("ghp_not-shown"));
    }
}
