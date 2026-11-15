//! Where packages are read from. Core does no I/O, so the binary supplies a directory
//! source, the built-in packages are embedded, and tests use [`MemSource`].

use std::collections::BTreeMap;

/// A tree of files. Paths are slash paths from its root, such as `demo/workflow.yaml`.
pub trait PackageSource: Send + Sync {
    /// Every file in the tree.
    fn list(&self) -> Result<Vec<String>, String>;
    /// One file's bytes.
    fn read(&self, path: &str) -> Result<Vec<u8>, String>;
}

/// One place packages load from: each top-level folder of `files` that holds a
/// `kiln-package.yaml` is a package. `label` names the source in paths and messages.
/// `builtin` marks the packages every build ships, which no one edits.
pub struct Source {
    pub label: String,
    pub builtin: bool,
    pub files: Box<dyn PackageSource>,
}

impl Source {
    pub fn new(
        label: impl Into<String>,
        builtin: bool,
        files: impl PackageSource + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            builtin,
            files: Box::new(files),
        }
    }
}

/// A source held in memory.
#[derive(Debug, Clone, Default)]
pub struct MemSource {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file, replacing one at the same path.
    pub fn with(mut self, path: &str, content: impl AsRef<[u8]>) -> Self {
        self.files
            .insert(path.to_owned(), content.as_ref().to_vec());
        self
    }

    pub fn insert(&mut self, path: &str, content: impl AsRef<[u8]>) {
        self.files
            .insert(path.to_owned(), content.as_ref().to_vec());
    }
}

impl PackageSource for MemSource {
    fn list(&self) -> Result<Vec<String>, String> {
        Ok(self.files.keys().cloned().collect())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path} is not in the source"))
    }
}

/// A source that cannot be listed, for the `source_unreadable` finding.
#[derive(Debug, Clone)]
pub struct BrokenSource(pub String);

impl PackageSource for BrokenSource {
    fn list(&self) -> Result<Vec<String>, String> {
        Err(self.0.clone())
    }

    fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err(self.0.clone())
    }
}

/// Something that loads the catalog each time it is asked, so an edited package counts from
/// the next call.
pub trait CatalogSource: Send + Sync {
    fn load(&self) -> crate::workflow::load::Catalog;
}

/// A [`CatalogSource`] over a list of sources.
pub struct Sources(pub Vec<Source>);

impl CatalogSource for Sources {
    fn load(&self) -> crate::workflow::load::Catalog {
        crate::workflow::load::load(&self.0)
    }
}
