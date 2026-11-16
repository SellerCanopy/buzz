//! The local secret store: one file per secret under `BUZZ_HIVE_SECRETS_DIR`, mode 0600, in
//! the paths [`SecretScope::path`] names. The host supervisor reads the same folder
//! (`BUZZ_HIVE_SUPERVISOR_SECRETS_DIR`). This is the only store local development and the
//! tests use.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use buzz_hive_core::ports::secrets::{SecretError, SecretScope, SecretSink, SecretString};

pub struct FileSecretSink {
    dir: PathBuf,
}

impl FileSecretSink {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

/// An agent folder name is lower case letters, digits and `-` or `_`; a variable name is
/// checked by the caller. Neither can walk out of the folder, and this says so again at the
/// one place that builds a path.
fn safe(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[async_trait::async_trait]
impl SecretSink for FileSecretSink {
    async fn put(
        &self,
        scope: SecretScope,
        var: &str,
        value: SecretString,
    ) -> Result<(), SecretError> {
        if !safe(var) || matches!(&scope, SecretScope::Agent(agent) if !safe(agent)) {
            return Err(SecretError::Unavailable(
                "the secret's name is not a safe file name".into(),
            ));
        }
        let path = self.dir.join(scope.path(var));
        let io = |what: &str, e: std::io::Error| {
            SecretError::Unavailable(format!("{what} {}: {e}", path.display()))
        };
        // The write is small, so it runs where it is called rather than on a blocking pool.
        let parent = path.parent().expect("a secret path has a folder");
        std::fs::create_dir_all(parent).map_err(|e| io("could not create the folder of", e))?;
        for folder in parent.ancestors().take_while(|p| *p != self.dir) {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| io("could not restrict", e))?;
        }
        // Written beside the file and renamed, so a reader never sees half a value.
        let temp = parent.join(format!(".{var}.tmp"));
        let written = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(value.expose().as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, &path)
        })();
        written.map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            io("could not write", e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_value_is_written_at_its_path_with_mode_0600_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let sink = FileSecretSink::new(dir.path().to_owned());
        sink.put(SecretScope::Team, "GH_TOKEN", SecretString::new("first"))
            .await
            .unwrap();
        sink.put(SecretScope::Team, "GH_TOKEN", SecretString::new("second"))
            .await
            .unwrap();
        sink.put(
            SecretScope::Agent("coder".into()),
            "DOCS_TOKEN",
            SecretString::new("own"),
        )
        .await
        .unwrap();
        let team = dir.path().join("team/env/GH_TOKEN");
        let own = dir.path().join("agents/coder/env/DOCS_TOKEN");
        assert_eq!(std::fs::read_to_string(&team).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "own");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&team), 0o600);
        assert_eq!(mode(&own), 0o600);
        assert_eq!(mode(&dir.path().join("team/env")), 0o700);
        assert_eq!(mode(&dir.path().join("agents/coder")), 0o700);
        // No temporary file is left behind.
        assert!(!dir.path().join("team/env/.GH_TOKEN.tmp").exists());
    }

    #[tokio::test]
    async fn a_name_that_could_leave_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sink = FileSecretSink::new(dir.path().to_owned());
        for (scope, var) in [
            (SecretScope::Team, "../escape"),
            (SecretScope::Team, "a/b"),
            (SecretScope::Team, ""),
            (SecretScope::Agent("../x".into()), "OK_VAR"),
            (SecretScope::Agent("a/b".into()), "OK_VAR"),
        ] {
            let error = sink
                .put(scope, var, SecretString::new("value-not-shown"))
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("value-not-shown"));
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_failed_write_says_where_and_never_shows_the_value() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        std::fs::write(dir.path().join("team"), "").unwrap();
        let sink = FileSecretSink::new(dir.path().to_owned());
        let error = sink
            .put(
                SecretScope::Team,
                "GH_TOKEN",
                SecretString::new("ghp_shown?"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("team"), "{error}");
        assert!(!error.contains("ghp_shown?"));
    }
}
