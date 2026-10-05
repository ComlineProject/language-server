//! The standard library in the editor: core's embedded `std` package, as
//! read-only files of every project view - seen under `std::…` and owned by
//! the "dependency" `std`, so resolution, hover, completion and
//! go-to-definition reach it like any dependency, and rename never edits it.
//! Its files are virtual (`comline-std:` URLs), never written anywhere.
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
/// `root/http.ids`, seen as `std::http`. And std's manifest, `root/config.idp`,
/// whose `//!` header documents the package itself.
pub fn files(root: &Url) -> Vec<SourceFile> {
    let schemas = stdlib::schemas().filter_map(|(namespace, source)| {
        let uri = root.join(&format!("{}.ids", namespace[1..].join("/"))).ok()?;
        Some(SourceFile::of_dependency(uri, source.to_string(), NAME, namespace))
    });
    let manifest = root
        .join("config.idp")
        .ok()
        .map(|uri| SourceFile::of_dependency(uri, stdlib::manifest().to_string(), NAME, vec![NAME.to_string()]));
    schemas.chain(manifest).collect()
}

/// The URL scheme std's files live under. They're never on disk: a client
/// shows one by asking the server for its text (`comline/stdSource`).
pub const SCHEME: &str = "comline-std";

/// The root every std file is under: `comline-std:/http.ids` is `std::http`.
/// No `//`: that's how editors (VS Code's `Uri`) print a URL with no
/// authority, so the URLs they send back match these exactly.
pub fn root() -> Url {
    Url::parse(&format!("{SCHEME}:/")).expect("a valid URL")
}

/// Whether `uri` names one of std's files.
pub fn is_std(uri: &Url) -> bool {
    uri.scheme() == SCHEME
}

/// The text of the std file at `uri`, for a client opening it. Matched by
/// path, so `comline-std:///http.ids` finds it too.
pub fn source(uri: &Url) -> Option<String> {
    if !is_std(uri) {
        return None;
    }
    files(&root()).into_iter().find(|file| file.uri.path() == uri.path()).map(|file| file.text)
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
        let files = files(&root());
        let named: Vec<(String, Vec<String>)> =
            files.iter().map(|f| (f.uri().to_string(), f.namespace().unwrap().to_vec())).collect();
        assert!(named.contains(&("comline-std:/http.ids".to_string(), vec!["std".to_string(), "http".to_string()])));
        assert!(named.contains(&("comline-std:/config.idp".to_string(), vec!["std".to_string()])), "the package's manifest");
        assert!(files.iter().all(|f| f.dependency() == Some(NAME)));
    }

    #[test]
    fn a_std_file_is_served_by_its_url() {
        let http = Url::parse("comline-std:/http.ids").unwrap();
        assert!(is_std(&http));
        assert!(source(&http).unwrap().contains("struct Request"));
        assert_eq!(source(&Url::parse("comline-std:///http.ids").unwrap()), source(&http));
        assert_eq!(source(&Url::parse("comline-std:///nope.ids").unwrap()), None);
        assert!(!is_std(&Url::parse("file:///pkg/src/http.ids").unwrap()));
    }
}
