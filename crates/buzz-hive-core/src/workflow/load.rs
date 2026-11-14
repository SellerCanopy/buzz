//! Reading packages from sources into a catalog.

use std::collections::BTreeMap;

use crate::domain::workflow::{
    Finding, PackageFile, Stage, WorkflowFile, PACKAGE_FILE, WORKFLOW_FILE,
};
use crate::workflow::decode::{decode_package, decode_workflow};
use crate::workflow::hash::hash_files;
use crate::workflow::source::Source;

/// Where a plugin names itself.
pub const PLUGIN_MANIFEST: &str = ".claude-plugin/plugin.json";

/// One loaded workflow package. `findings` holds the problems found while reading its
/// files; the check reports them with the rest.
#[derive(Debug, Clone)]
pub struct Package {
    pub meta: PackageFile,
    pub workflow: WorkflowFile,
    /// `<source label>/<folder>`, for messages.
    pub path: String,
    /// Every file in the package, by slash path inside it.
    pub files: BTreeMap<String, String>,
    pub findings: Vec<Finding>,
    /// Loaded from a built-in source.
    pub builtin: bool,
}

impl Package {
    /// The stage with `id` and its position in file order.
    pub fn stage(&self, id: &str) -> Option<(&Stage, usize)> {
        self.workflow
            .stages
            .iter()
            .enumerate()
            .find(|(_, s)| s.id == id)
            .map(|(i, s)| (s, i))
    }

    /// The name before the colon in a plugin call: the plugin manifest's name when it has
    /// one, else the package name.
    pub fn command_prefix(&self) -> String {
        let manifest = self
            .files
            .get(PLUGIN_MANIFEST)
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|json| json.get("name").and_then(|n| n.as_str().map(str::to_owned)))
            .filter(|name| !name.is_empty());
        manifest.unwrap_or_else(|| self.meta.name.clone())
    }

    /// A stage's prompt: the slash line of its command and args, its prompt field, or the
    /// contents of its prompt_file inside the package.
    pub fn prompt_text(&self, stage: &Stage) -> Result<String, String> {
        if !stage.command.is_empty() {
            let mut line = format!("/{}:{}", self.command_prefix(), stage.command);
            if !stage.args.is_empty() {
                line.push(' ');
                line.push_str(&stage.args);
            }
            return Ok(line);
        }
        if stage.prompt_file.is_empty() {
            return Ok(stage.prompt.clone());
        }
        self.files
            .get(&clean_path(&stage.prompt_file))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{}: stage {}: prompt_file {} is not in the package",
                    self.path, stage.id, stage.prompt_file
                )
            })
    }

    /// The hash of the package's files.
    pub fn hash(&self) -> String {
        hash_files(&self.files)
    }
}

/// Go's `path.Clean`: slashes collapse, `.` goes, `..` cancels the part before it.
pub fn clean_path(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                _ if rooted => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// Go's `fs.ValidPath`: a path inside a tree, with no `.` or `..` part, no empty part and
/// no leading or trailing slash. A lone `.` is valid.
pub fn valid_path(path: &str) -> bool {
    if path == "." {
        return true;
    }
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Every package loaded from a set of sources. Packages sharing a name are all kept, so
/// the checks can report the clash on each of them.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    by_name: BTreeMap<String, Vec<Package>>,
    /// The folders that could not load as a package at all.
    pub findings: Vec<Finding>,
}

impl Catalog {
    /// The first package loaded with `name`.
    pub fn get(&self, name: &str) -> Option<&Package> {
        self.by_name.get(name).and_then(|packages| packages.first())
    }

    /// Every package loaded with `name`, in load order.
    pub fn all(&self, name: &str) -> &[Package] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// Every package name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(String::as_str).collect()
    }

    /// Adds a package, for tests and for callers that build a catalog by hand.
    pub fn insert(&mut self, package: Package) {
        self.by_name
            .entry(package.meta.name.clone())
            .or_default()
            .push(package);
    }
}

/// Version-control and Finder leftovers no package keeps, at any depth.
pub fn is_junk_name(name: &str) -> bool {
    matches!(name, ".git" | ".DS_Store" | ".svn" | ".hg")
}

/// Reads every package in `sources`, in order. A folder whose `kiln-package.yaml` gives no
/// name becomes a catalog finding and is left out; every other problem stays with its
/// package.
pub fn load(sources: &[Source]) -> Catalog {
    let mut catalog = Catalog::default();
    for source in sources {
        let listed = match source.files.list() {
            Ok(listed) => listed,
            Err(error) => {
                catalog
                    .findings
                    .push(Finding::error(&source.label, "source_unreadable", error));
                continue;
            }
        };
        // Group the files by their top-level folder.
        let mut folders: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for path in &listed {
            let Some((folder, rest)) = path.split_once('/') else {
                continue;
            };
            if folder.is_empty() || folder.starts_with('.') || rest.is_empty() {
                continue;
            }
            folders.entry(folder).or_default().push(path);
        }
        for (folder, paths) in folders {
            let package_file = format!("{folder}/{PACKAGE_FILE}");
            if !paths.contains(&package_file.as_str()) {
                continue;
            }
            let where_ = format!("{}/{folder}", source.label);
            let mut files = BTreeMap::new();
            let mut failed = None;
            for path in paths {
                let inside = &path[folder.len() + 1..];
                if inside.split('/').any(is_junk_name) {
                    continue;
                }
                match source.files.read(path) {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => {
                            files.insert(inside.to_owned(), text);
                        }
                        Err(_) => {
                            failed = Some(format!("{inside} is not UTF-8 text"));
                            break;
                        }
                    },
                    Err(error) => {
                        failed = Some(error);
                        break;
                    }
                }
            }
            if let Some(error) = failed {
                catalog
                    .findings
                    .push(Finding::error(&where_, "package_unreadable", error));
                continue;
            }
            match parse(&where_, files) {
                Ok(mut package) => {
                    package.builtin = source.builtin;
                    catalog.insert(package);
                }
                Err(error) => {
                    catalog
                        .findings
                        .push(Finding::error(&where_, "package_unreadable", error));
                }
            }
        }
    }
    catalog
}

/// Builds a package from its files. It fails only when `kiln-package.yaml` gives no name;
/// every other problem becomes one of the package's findings.
pub fn parse(where_: &str, files: BTreeMap<String, String>) -> Result<Package, String> {
    let Some(meta_text) = files.get(PACKAGE_FILE) else {
        return Err(format!("{PACKAGE_FILE} is missing"));
    };
    let (meta, mut findings) = decode_package(meta_text);
    if meta.name.is_empty() {
        return Err(format!("{PACKAGE_FILE}: name is empty or unreadable"));
    }
    let mut workflow = WorkflowFile::default();
    match files.get(WORKFLOW_FILE) {
        None => findings.push(Finding::error(
            WORKFLOW_FILE,
            "workflow_missing",
            format!("{WORKFLOW_FILE} is missing"),
        )),
        Some(text) => {
            let (decoded, more) = decode_workflow(text);
            workflow = decoded;
            findings.extend(more);
        }
    }
    Ok(Package {
        meta,
        workflow,
        path: where_.to_owned(),
        files,
        findings,
        builtin: false,
    })
}
