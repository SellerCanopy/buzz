//! A folder on disk as a source of workflow packages.

use std::fs;
use std::path::{Path, PathBuf};

use buzz_hive_core::workflow::load::is_junk_name;
use buzz_hive_core::workflow::source::PackageSource;

/// The largest file a package may hold. A prompt is text, so this is generous.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Every regular file under `root`, by slash path. Symbolic links, version-control folders
/// and Finder leftovers are skipped, so a package never reads outside its folder.
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl PackageSource for DirSource {
    fn list(&self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        walk(&self.root, "", &mut out)?;
        out.sort();
        Ok(out)
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let full = self.root.join(path);
        let meta = fs::symlink_metadata(&full).map_err(|e| format!("{path}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{path} is not a regular file"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("{path} is over {MAX_FILE_BYTES} bytes"));
        }
        fs::read(&full).map_err(|e| format!("{path}: {e}"))
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_junk_name(&name) {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            walk(&entry.path(), &path, out)?;
        } else if kind.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    #[test]
    fn lists_regular_files_and_skips_junk_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "demo/workflow.yaml", "version: 2\n");
        write(root, "demo/prompts/a.md", "Do the work.\n");
        write(root, "demo/.git/config", "secret\n");
        write(root, "demo/.DS_Store", "junk\n");
        write(root, "demo/.hidden/x", "kept\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("demo/workflow.yaml"), root.join("demo/link.yaml"))
            .unwrap();
        let source = DirSource::new(root);
        assert_eq!(
            source.list().unwrap(),
            ["demo/.hidden/x", "demo/prompts/a.md", "demo/workflow.yaml"]
        );
        assert_eq!(source.read("demo/prompts/a.md").unwrap(), b"Do the work.\n");
    }

    #[test]
    fn refuses_a_file_over_the_cap_and_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "big.md", &"x".repeat(MAX_FILE_BYTES as usize + 1));
        let source = DirSource::new(root);
        assert!(source.read("big.md").unwrap_err().contains("over"));
        #[cfg(unix)]
        {
            write(root, "real.md", "x");
            std::os::unix::fs::symlink(root.join("real.md"), root.join("link.md")).unwrap();
            assert!(source.read("link.md").is_err());
        }
    }

    #[test]
    fn a_missing_folder_cannot_be_listed() {
        let source = DirSource::new("/nonexistent/buzz-hive-workflows");
        assert!(source.list().is_err());
    }
}
