//! Parsed trees, reused across requests until a file's text changes.
//!
//! Every request (and every diagnostics refresh, on each keystroke) builds a
//! [`crate::analysis::project::Project`] over the whole package, and parsing
//! dominates that cost - about 45ms for a 200-file package in a release
//! build, against about 3ms for every file's diagnostics. Keeping the last
//! parse per file makes a rebuild cost only the files that changed.
//!
//! One entry per file (the latest text replaces the previous one), so it is
//! bounded by the number of files ever seen. Files that don't parse aren't
//! kept - callers that need the parse errors parse those themselves.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use comline_core::schema::idl::grammar::Document;
use lsp_types::Url;

use crate::parser;

struct Entry {
    len: usize,
    hash: u64,
    document: Arc<Document>,
}

fn cache() -> &'static Mutex<HashMap<Url, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<Url, Entry>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn hash(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// The parsed tree of `source` (the text of `uri`), or `None` when it
/// doesn't parse.
pub fn parse(uri: &Url, source: &str) -> Option<Arc<Document>> {
    let hash = hash(source);

    if let Some(entry) = cache().lock().ok()?.get(uri) {
        if entry.len == source.len() && entry.hash == hash {
            return Some(Arc::clone(&entry.document));
        }
    }

    let document = Arc::new(parser::parse(source).ok()?.document?);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(uri.clone(), Entry { len: source.len(), hash, document: Arc::clone(&document) });
    }
    Some(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_reuses_the_tree_and_new_text_replaces_it() {
        let uri = Url::parse("file:///parse-cache-test/src/a.ids").unwrap();

        let first = parse(&uri, "struct A {\n    x: bool\n}\n").unwrap();
        let again = parse(&uri, "struct A {\n    x: bool\n}\n").unwrap();
        assert!(Arc::ptr_eq(&first, &again));

        let changed = parse(&uri, "struct B {\n    y: bool\n}\n").unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.0.len(), 1);
    }

    #[test]
    fn text_that_does_not_parse_is_none() {
        let uri = Url::parse("file:///parse-cache-test/src/broken.ids").unwrap();
        assert!(parse(&uri, "struct {").is_none());
    }
}
