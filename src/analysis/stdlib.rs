//! The standard library in the editor: core's embedded `std` package, as
//! read-only files of every project view - seen under `std::…` and owned by
//! the "dependency" `std`, so resolution, hover, completion and
//! go-to-definition reach it like any dependency, and rename never edits it.
//!
//! The editor has all of std at hand (completion lists it); a build merges
//! only what a package imports (`comline_core::package::stdlib`).

use comline_core::package::stdlib;
use lsp_types::Url;

use crate::analysis::source::SourceFile;

/// The dependency name std's files are owned by, and their first namespace
/// segment.
pub const NAME: &str = stdlib::NAMESPACE;

/// Every std schema as a file under `root` (a directory URL, ending in `/`):
/// `root/http.ids`, seen as `std::http`.
pub fn files(root: &Url) -> Vec<SourceFile> {
    stdlib::schemas()
        .filter_map(|(namespace, source)| {
            let uri = root.join(&format!("{}.ids", namespace[1..].join("/"))).ok()?;
            Some(SourceFile::of_dependency(uri, source.to_string(), NAME, namespace))
        })
        .collect()
}

/// Where std's files live when they can't be on disk (the playground): a
/// URL no client opens, but every analysis can name.
pub fn virtual_root() -> Url {
    Url::parse("comline-std:///").expect("a valid URL")
}

/// How a hover or completion detail names what a file belongs to:
/// `std`, or ``dependency `shared` ``.
pub fn owner(dependency: &str) -> String {
    match dependency == NAME {
        true => NAME.to_string(),
        false => format!("dependency `{dependency}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::source::ProjectSource;

    #[test]
    fn std_files_are_named_by_their_std_namespace() {
        let files = files(&virtual_root());
        let named: Vec<(String, Vec<String>)> =
            files.iter().map(|f| (f.uri().to_string(), f.namespace().unwrap().to_vec())).collect();
        assert!(named.contains(&("comline-std:///http.ids".to_string(), vec!["std".to_string(), "http".to_string()])));
        assert!(files.iter().all(|f| f.dependency() == Some(NAME)));
    }
}
