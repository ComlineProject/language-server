//! Every `.ids` schema under the workspace folders, as last read from disk -
//! so files that aren't open still take part in import checks, completion,
//! go-to-definition, references and rename.
//!
//! Only files inside a package's `src/` directory are indexed (anything else
//! has no namespace to be imported by), and each request only sees the files
//! of its own package: two packages that both have a `src/types.ids` never
//! see each other's. The client keeps the index current through
//! `workspace/didChangeWatchedFiles` (`comline-vscode` watches
//! `**/*.{ids,idp}`); an open buffer always wins over the disk copy.
//!
//! Each package's dependencies are indexed too (see `dependencies`), kept
//! apart: they're seen under the dependency's name, and only by the package
//! that declares them.

use std::path::{Path, PathBuf};

use comline_core::package::layout;
use dashmap::DashMap;
use lsp_types::Url;

use crate::dependencies::{self, IndexedDependency};

/// Directories never worth descending into: build output, dependency
/// caches, VCS metadata (hidden directories are skipped too).
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

/// Path components that put a file outside the package index, even when it
/// arrives through the file watcher: the deps cache lives under `.comline/`.
const EXCLUDED_COMPONENTS: &[&str] = &[".comline", ".git", "target", "node_modules"];

/// More than any real workspace has; stops a scan of a huge unrelated tree.
const MAX_FILES: usize = 20_000;

#[derive(Default)]
pub struct WorkspaceIndex {
    files: DashMap<Url, String>,
    /// Each package's dependencies, keyed like [`package_root`].
    dependencies: DashMap<String, Vec<IndexedDependency>>,
}

impl WorkspaceIndex {
    /// Index every schema under `roots`, replacing whatever was indexed.
    pub fn scan(&self, roots: &[PathBuf]) {
        self.files.clear();
        let mut found = Vec::new();
        for root in roots {
            collect_schemas(root, &mut found);
        }
        for path in found.into_iter().take(MAX_FILES) {
            self.refresh_path(&path);
        }
    }

    /// Re-read one file from disk - created or changed - or forget it if
    /// it's gone or isn't a package schema.
    pub fn refresh(&self, uri: &Url) {
        match uri.to_file_path() {
            Ok(path) => self.refresh_path(&path),
            Err(()) => self.remove(uri),
        }
    }

    pub fn remove(&self, uri: &Url) {
        self.files.remove(uri);
        // The client's spelling of a URI can differ from `from_file_path`'s
        // (`c%3A` vs `C:` on Windows); match by path too.
        if let Some(key) = path_key(uri) {
            self.files.retain(|u, _| path_key(u).as_ref() != Some(&key));
        }
    }

    fn refresh_path(&self, path: &Path) {
        let Ok(uri) = Url::from_file_path(path) else { return };
        let is_schema = path.extension().is_some_and(|e| e == "ids")
            && layout::schemas_root_for(path).is_some()
            && !path.components().any(|c| EXCLUDED_COMPONENTS.contains(&c.as_os_str().to_string_lossy().as_ref()));

        match std::fs::read_to_string(path) {
            Ok(text) if is_schema => {
                self.files.insert(uri, text);
            }
            _ => self.remove(&uri),
        }
    }

    /// Every indexed file in the same package as `uri` (the same `src/`
    /// root), except `uri` itself.
    pub fn package_files(&self, uri: &Url) -> Vec<(Url, String)> {
        let root = package_root(uri);
        let own = path_key(uri);
        self.files
            .iter()
            .filter(|e| package_root(e.key()) == root && path_key(e.key()) != own)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    /// Every package (its `src/` directory) that has an indexed schema.
    pub fn package_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self
            .files
            .iter()
            .filter_map(|e| Some(layout::schemas_root_for(&e.key().to_file_path().ok()?)?.to_path_buf()))
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    /// Re-read the dependencies of the package whose `src/` is `src_root`,
    /// from `manifest` (its `config.idp`'s text) or, when `None`, the file on
    /// disk. A package without a manifest has none.
    pub fn index_dependencies(&self, src_root: &Path, manifest: Option<&str>) {
        let Some(package_dir) = src_root.parent() else { return };
        let on_disk;
        let manifest = match manifest {
            Some(text) => text,
            None => {
                on_disk = std::fs::read_to_string(package_dir.join(dependencies::MANIFEST)).unwrap_or_default();
                &on_disk
            }
        };
        self.dependencies.insert(normalize(src_root), dependencies::index(package_dir, manifest));
    }

    /// The dependencies of `uri`'s package, indexing them first if no one
    /// has yet (a file outside the scanned workspace folders).
    pub fn dependencies_of(&self, uri: &Url) -> Vec<IndexedDependency> {
        let Some(src_root) = uri.to_file_path().ok().and_then(|p| layout::schemas_root_for(&p).map(Path::to_path_buf))
        else {
            return vec![];
        };
        let key = normalize(&src_root);
        if !self.dependencies.contains_key(&key) {
            self.index_dependencies(&src_root, None);
        }
        self.dependencies.get(&key).map(|d| d.clone()).unwrap_or_default()
    }

    /// Every package (its `src/` directory) one of whose dependencies lives
    /// at or above `path` - the ones to re-index when `path` changes.
    pub fn packages_depending_on(&self, path: &Path) -> Vec<PathBuf> {
        self.dependencies
            .iter()
            .filter(|e| e.value().iter().any(|d| d.dir.as_ref().is_some_and(|dir| path.starts_with(dir))))
            .map(|e| PathBuf::from(e.key()))
            .collect()
    }

    /// Every indexed file, all packages.
    pub fn all_files(&self) -> Vec<(Url, String)> {
        self.files.iter().map(|e| (e.key().clone(), e.value().clone())).collect()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The package a file belongs to: its `src/` directory, `None` for a file
/// outside any (a loose or unsaved file).
pub fn package_root(uri: &Url) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    layout::schemas_root_for(&path).map(normalize)
}

/// A comparable key for the file a URI names, independent of how it's
/// spelled (percent-encoding, drive-letter case).
pub fn path_key(uri: &Url) -> Option<String> {
    uri.to_file_path().ok().map(|p| normalize(&p))
}

fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Every `.ids` file under `dir` that lives in some `src/` directory.
/// Symlinked directories aren't followed (no cycles, no escaping the root).
fn collect_schemas(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        if found.len() >= MAX_FILES {
            return;
        }
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if file_type.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                collect_schemas(&path, found);
            }
        } else if file_type.is_file()
            && path.extension().is_some_and(|e| e == "ids")
            && layout::schemas_root_for(&path).is_some()
        {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway workspace: two packages that both have `src/types.ids`,
    /// a schema outside any `src/`, and a `target/` directory.
    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("comline-lsp-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in [
            ("a/config.idp", "congregation a\n"),
            ("a/src/types.ids", "struct A {\n    x: bool\n}\n"),
            ("a/src/chat/admin.ids", "struct Admin {\n    x: bool\n}\n"),
            ("b/src/types.ids", "struct B {\n    x: bool\n}\n"),
            ("loose.ids", "struct Loose {\n    x: bool\n}\n"),
            ("a/target/src/stale.ids", "struct Stale {\n    x: bool\n}\n"),
            ("a/.comline/src/hidden.ids", "struct Hidden {\n    x: bool\n}\n"),
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn names(files: Vec<(Url, String)>) -> Vec<String> {
        let mut names: Vec<_> = files.into_iter().map(|(u, _)| u.path().rsplit("/src/").next().unwrap().to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn scans_package_schemas_only_and_scopes_them_per_package() {
        let root = workspace("scan");
        let index = WorkspaceIndex::default();
        index.scan(std::slice::from_ref(&root));

        assert_eq!(index.len(), 3, "a/types, a/chat/admin, b/types: {:?}", names(index.all_files()));

        let a_types = Url::from_file_path(root.join("a/src/types.ids")).unwrap();
        assert_eq!(names(index.package_files(&a_types)), vec!["chat/admin.ids"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_picks_up_new_changed_and_deleted_files() {
        let root = workspace("refresh").join("refresh");
        std::fs::create_dir_all(root.join("p/src")).unwrap();
        let index = WorkspaceIndex::default();
        index.scan(std::slice::from_ref(&root));
        assert!(index.is_empty());

        let file = root.join("p/src/new.ids");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, "struct New {\n    x: bool\n}\n").unwrap();
        index.refresh(&uri);
        assert_eq!(index.len(), 1);

        std::fs::write(&file, "struct Changed {\n    x: bool\n}\n").unwrap();
        index.refresh(&uri);
        assert!(index.all_files()[0].1.contains("Changed"));

        std::fs::remove_file(&file).unwrap();
        index.refresh(&uri);
        assert!(index.is_empty());

        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
