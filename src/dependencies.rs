//! A package's dependencies as the editor finds them on disk - never fetched.
//!
//! Each dependency `config.idp` declares is located with core's own
//! `DependencyConfig::package_dir`: a `Path` dependency's directory, or a
//! `Git` pin's checkout in `.comline/deps-cache/` (there once `comline check`
//! has fetched it). Its schemas are read from there and handed to the analysis
//! under the dependency's name (`shared_types::models`), the way the build
//! merges them. A dependency with nothing on disk yet (not fetched, a missing
//! path, a registry source) has no files: imports of it aren't checked, and
//! its entry in `config.idp` says why.

use std::path::{Path, PathBuf};

use comline_core::package::config::dependency::{DependencyConfig, DependencySource};
use comline_core::package::layout::SCHEMAS_DIR;
use lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::parser;
use crate::util::{byte_range_to_lsp_range, word_occurrences};

/// The file every package's manifest is in.
pub const MANIFEST: &str = "config.idp";

/// The directory std's schemas are opened from (go-to-definition, hover
/// links): a copy of core's embedded std, written once per std version under
/// the system temp directory, read-only. The virtual root if that can't be
/// written - std still resolves, it just can't be opened.
pub fn std_root() -> Url {
    let sources: String = comline_core::package::stdlib::schemas().map(|(_, source)| source).collect();
    let version = comline_core::package::build::cas::storage::Hash::from_bytes(sources.as_bytes()).to_hex();
    let dir = std::env::temp_dir().join(format!("comline-std-{}", &version[..16]));

    let written = crate::analysis::stdlib::files(&crate::analysis::stdlib::virtual_root()).iter().all(|file| {
        let path = dir.join(file.uri.path().trim_start_matches('/'));
        if path.exists() {
            return true;
        }
        let ok = path.parent().is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
            && std::fs::write(&path, &file.text).is_ok();
        if ok {
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                let _ = std::fs::set_permissions(&path, permissions);
            }
        }
        ok
    });

    match written {
        true => Url::from_directory_path(&dir).unwrap_or_else(|_| crate::analysis::stdlib::virtual_root()),
        false => crate::analysis::stdlib::virtual_root(),
    }
}

/// One declared dependency and what's on disk for it.
#[derive(Debug, Clone)]
pub struct IndexedDependency {
    pub name: String,
    /// Where its package lives (`None` for a registry source).
    pub dir: Option<PathBuf>,
    /// Its schemas, from `dir/src`.
    pub files: Vec<(Url, String)>,
}

/// Every dependency `manifest` (the text of `package_dir/config.idp`)
/// declares, with the schemas of the ones whose package is on disk.
pub fn index(package_dir: &Path, manifest: &str) -> Vec<IndexedDependency> {
    let mut indexed: Vec<IndexedDependency> = declared(manifest)
        .into_iter()
        .map(|dependency| {
            // Canonical, so `../shared` matches the paths file events carry
            // and go-to-definition lands on a clean path.
            let dir = dependency.package_dir(package_dir).map(|dir| dir.canonicalize().unwrap_or(dir));
            let files = dir.as_ref().map(|dir| read_schemas(&dir.join(SCHEMAS_DIR))).unwrap_or_default();
            IndexedDependency { name: dependency.name, dir, files }
        })
        .collect();
    indexed.sort_by(|a, b| a.name.cmp(&b.name));
    indexed
}

/// Diagnostics for `config.idp`'s dependency entries that won't resolve -
/// the ones `comline check` would stop on, or that it has yet to fetch.
pub fn manifest_diagnostics(package_dir: &Path, manifest: &str) -> Vec<Diagnostic> {
    let Some(congregation) = parser::parse_idp(manifest).ok().and_then(|r| r.document) else {
        return vec![];
    };
    let dependencies = match DependencyConfig::parse_dependencies(&congregation.assignments) {
        Ok(dependencies) => dependencies,
        Err(message) => {
            let at = manifest.find("dependencies").map(|i| (i, i + "dependencies".len())).unwrap_or((0, 0));
            return vec![diagnostic(manifest, at, DiagnosticSeverity::ERROR, message)];
        }
    };

    let mut names: Vec<&String> = dependencies.keys().collect();
    names.sort();

    names
        .into_iter()
        .filter_map(|name| {
            let dependency = &dependencies[name];
            let (severity, message) = problem(package_dir, dependency)?;
            Some(diagnostic(manifest, entry_range(manifest, name), severity, message))
        })
        .collect()
}

fn problem(package_dir: &Path, dependency: &DependencyConfig) -> Option<(DiagnosticSeverity, String)> {
    let dir = dependency.package_dir(package_dir);
    match (&dependency.source, dir) {
        (DependencySource::Registry { .. }, _) | (_, None) => Some((
            DiagnosticSeverity::WARNING,
            "registry dependencies aren't supported yet — `comline check` rejects this entry".to_string(),
        )),
        (DependencySource::Git { .. }, Some(dir)) if !dir.exists() => Some((
            DiagnosticSeverity::WARNING,
            "not fetched yet — run `comline check` to fetch it; until then its imports aren't checked".to_string(),
        )),
        (DependencySource::Path { path, .. }, Some(dir)) if !dir.exists() => {
            Some((DiagnosticSeverity::ERROR, format!("`{}` doesn't exist", path.display())))
        }
        (DependencySource::Path { path, .. }, Some(dir)) if !dir.join(MANIFEST).exists() => Some((
            DiagnosticSeverity::ERROR,
            format!("`{}` isn't a Comline package (no `{MANIFEST}` there)", path.display()),
        )),
        (_, Some(dir)) if read_schemas(&dir.join(SCHEMAS_DIR)).is_empty() => Some((
            DiagnosticSeverity::WARNING,
            format!("has no schemas in `{SCHEMAS_DIR}/` — nothing to import"),
        )),
        _ => None,
    }
}

fn declared(manifest: &str) -> Vec<DependencyConfig> {
    parser::parse_idp(manifest)
        .ok()
        .and_then(|r| r.document)
        .and_then(|congregation| DependencyConfig::parse_dependencies(&congregation.assignments).ok())
        .map(|dependencies| dependencies.into_values().collect())
        .unwrap_or_default()
}

/// The range of a dependency entry's name: the first `name =` after the
/// `dependencies` keyword.
fn entry_range(manifest: &str, name: &str) -> (usize, usize) {
    let from = manifest.find("dependencies").unwrap_or(0);
    word_occurrences(&manifest[from..], name)
        .into_iter()
        .map(|i| from + i)
        .find(|&i| manifest[i + name.len()..].trim_start().starts_with('='))
        .map(|i| (i, i + name.len()))
        .unwrap_or((from, from))
}

fn diagnostic(source: &str, range: (usize, usize), severity: DiagnosticSeverity, message: String) -> Diagnostic {
    Diagnostic {
        range: byte_range_to_lsp_range(source, range.0, range.1),
        severity: Some(severity),
        source: Some("comline".to_string()),
        message,
        ..Default::default()
    }
}

/// Every `.ids` file under `dir`, with its text.
fn read_schemas(dir: &Path) -> Vec<(Url, String)> {
    let mut paths = Vec::new();
    collect(dir, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| Some((Url::from_file_path(&path).ok()?, std::fs::read_to_string(&path).ok()?)))
        .collect()
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        if file_type.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            collect(&path, found);
        } else if file_type.is_file() && path.extension().is_some_and(|e| e == "ids") {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use comline_core::package::config::dependency::git_checkout_dir;

    const CONSUMER: &str = "congregation app\nspecification_version = 1\n\n\
        dependencies = {\n    \
            shared = {\n        path = \"../shared\"\n    }\n    \
            fetched = {\n        version = \"1.0.0\"\n        uri = \"https://example.test/fetched\"\n        commit = \"abc\"\n    }\n    \
            pending = {\n        version = \"1.0.0\"\n        uri = \"https://example.test/pending\"\n        commit = \"def\"\n    }\n    \
            gone = {\n        path = \"../gone\"\n    }\n    \
            hosted = {\n        version = \"1.0.0\"\n        uri = \"comline://example.test/hosted\"\n    }\n\
        }\n";

    /// A consumer package next to a `shared` path dependency, with one git
    /// pin already in the deps cache and one not.
    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("comline-lsp-deps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let consumer = root.join("app");
        let fetched = git_checkout_dir(&consumer.join(".comline/deps-cache"), "https://example.test/fetched", "abc");
        for (path, text) in [
            (consumer.join("config.idp"), CONSUMER),
            (consumer.join("src/main.ids"), "struct Main {\n    id: u64\n}\n"),
            (root.join("shared/config.idp"), "congregation shared\nspecification_version = 1\n"),
            (root.join("shared/src/models.ids"), "struct Thing {\n    id: u64\n}\n"),
            (fetched.join("config.idp"), "congregation fetched\nspecification_version = 1\n"),
            (fetched.join("src/wire/frame.ids"), "struct Frame {\n    len: u32\n}\n"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    #[test]
    fn indexes_path_and_fetched_git_dependencies_only() {
        let root = workspace("index");
        let indexed = index(&root.join("app"), CONSUMER);

        let files: Vec<(&str, usize)> = indexed.iter().map(|d| (d.name.as_str(), d.files.len())).collect();
        assert_eq!(files, vec![("fetched", 1), ("gone", 0), ("hosted", 0), ("pending", 0), ("shared", 1)]);
        assert!(indexed[0].files[0].0.path().ends_with("/src/wire/frame.ids"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_diagnostics_name_each_entry_that_wont_resolve() {
        let root = workspace("manifest");
        let found: Vec<(String, String)> = manifest_diagnostics(&root.join("app"), CONSUMER)
            .into_iter()
            .map(|d| {
                let line = CONSUMER.lines().nth(d.range.start.line as usize).unwrap();
                let name = &line[d.range.start.character as usize..d.range.end.character as usize];
                (name.to_string(), d.message)
            })
            .collect();

        assert_eq!(
            found,
            vec![
                ("gone".to_string(), "`../gone` doesn't exist".to_string()),
                (
                    "hosted".to_string(),
                    "registry dependencies aren't supported yet — `comline check` rejects this entry".to_string()
                ),
                (
                    "pending".to_string(),
                    "not fetched yet — run `comline check` to fetch it; until then its imports aren't checked"
                        .to_string()
                ),
            ]
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
