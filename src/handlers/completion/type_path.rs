//! Completion in a type position along a path (core#290): `types::` after
//! `use package::types`, `package::billing::`, `b::` after
//! `use package::billing::api as b`, `std::`.
//!
//! A qualified type starts with a name in scope or a root: `package`, `parent`,
//! `self`, `std` or a dependency's name (see [`crate::analysis::names`]). What
//! follows the `::` is what is under that namespace: its modules, and the types
//! its schema declares (a path in a type position ends at a type, not at a
//! module). The first segment is read the way core reads it: a name a
//! whole-namespace `use` of this file brought in comes before a root, then a
//! root, and nothing else (a declaration of this file, or a name a `use`
//! imports, is no qualifier).
//!
//! Also the roots themselves, offered along with the names in scope where a
//! type is expected, which complete to `root::` and ask for what comes next.

use lsp_types::{CompletionItem, CompletionTextEdit, Range, TextEdit, Url};

use crate::analysis::imports;
use crate::analysis::modules::{self, ChildModule, DeclKind};
use crate::analysis::names;
use crate::analysis::project::Project;
use crate::analysis::source::ProjectSource;
use crate::analysis::symbols;
use crate::handlers::completion::use_path;
use crate::parser;
use crate::util::{byte_range_to_lsp_range, is_ident_char, is_identifier};

/// The path written before the word being typed in a type position.
#[derive(Debug, PartialEq)]
pub(super) struct TypePath {
    /// The segments before the one being typed (`types` for `types::Us`).
    pub segments: Vec<String>,
    /// Byte offset where the segment being typed starts.
    pub partial_start: usize,
}

/// Where the word ending at `offset` starts.
pub(super) fn word_start(source: &str, offset: usize) -> usize {
    source[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(offset, |(i, _)| i)
}

/// The path before the word being typed at `offset`, when there is one:
/// `types::` and `types::Us` both say `["types"]`. `None` where the word has no
/// `::` in front of it.
pub(super) fn path_before(source: &str, offset: usize) -> Option<TypePath> {
    let partial_start = word_start(source, offset);
    let mut segments = vec![];
    let mut rest = &source[..partial_start];
    while let Some(before) = rest.strip_suffix("::") {
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| !is_ident_char(*c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let segment = &before[start..];
        if !is_identifier(segment) {
            return None;
        }
        segments.insert(0, segment.to_string());
        rest = &before[..start];
    }
    (!segments.is_empty()).then_some(TypePath {
        segments,
        partial_start,
    })
}

/// What can come next in `path`, written in the file at `uri` (its text
/// `source`, the cursor at `offset`), with `other_files` the rest of its
/// package.
pub(super) fn completions<S: ProjectSource>(
    path: &TypePath,
    source: &str,
    uri: &Url,
    offset: usize,
    other_files: &[S],
) -> Vec<CompletionItem> {
    let project = Project::new(other_files.iter());
    let range = byte_range_to_lsp_range(source, path.partial_start, offset);
    let first = &path.segments[0];

    // A name a whole-namespace `use` here brought in comes first, as it does
    // in core's scope; then a root.
    let bindings = names::namespace_bindings(&project, &super::uses_in(source, uri));
    let (base, relative) = match bindings.iter().find(|(name, _)| name == first) {
        Some((_, namespace)) => (
            namespace
                .iter()
                .chain(&path.segments[1..])
                .cloned()
                .collect::<Vec<_>>(),
            false,
        ),
        None => {
            let relative = modules::is_relative_prefix(first);
            if !relative && !use_path::leaves_the_package(&project, first) {
                return vec![];
            }
            match modules::resolve_path(&path.segments, false, uri) {
                Some(base) => (base, relative),
                None => return vec![], // `parent::` above the top
            }
        }
    };

    // A relative path stays inside the package.
    let schema = project
        .docs
        .iter()
        .find(|d| d.namespace == base && (!relative || d.dependency.is_none()));
    let mut items: Vec<CompletionItem> = modules::children(&project, &base, relative)
        .iter()
        .map(|child| module_item(child, range))
        .collect();
    if let Some(schema) = schema {
        items.extend(
            modules::declarations(schema)
                .iter()
                .filter(|declared| {
                    matches!(
                        declared.kind,
                        DeclKind::Struct | DeclKind::Enum | DeclKind::Type
                    )
                })
                .map(|declared| use_path::declared_item(declared, range)),
        );
    }
    // The file itself (`self::`, or its own path) isn't among the other files:
    // what it declares is read from it without the line being typed, which is
    // what keeps it from parsing.
    if base == imports::namespace_of(uri) {
        let blanked = super::blank_line_at(source, offset);
        if let Some(document) = parser::parse_document(&blanked) {
            let table = symbols::build_symbol_table(&document, uri, &blanked);
            items.extend(super::get_type_completions(Some(&table), &blanked));
        }
    }
    items
}

/// A module one level down. A path in a type position ends at a type, so a
/// schema is gone through as a directory is: `::` is inserted too.
fn module_item(child: &ChildModule, range: Range) -> CompletionItem {
    let mut item = use_path::child_item(child, range);
    if child.schema {
        item.text_edit = Some(CompletionTextEdit::Edit(TextEdit {
            range,
            new_text: format!("{}::", child.name),
        }));
        item.command = Some(use_path::trigger_suggest());
    }
    item
}

/// The roots a path in a type position can start with, after the names in
/// scope: the package's dependencies, `self`, `parent`, `package` and `std`.
pub(super) fn roots<S: ProjectSource>(
    source: &str,
    offset: usize,
    other_files: &[S],
) -> Vec<CompletionItem> {
    let project = Project::new(other_files.iter());
    let range = byte_range_to_lsp_range(source, word_start(source, offset), offset);
    use_path::path_starts(&project, false, range)
        .into_iter()
        .map(|mut item| {
            item.sort_text = Some(format!("~0{}", item.label));
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_path_before_the_word() {
        let at = |source: &str| path_before(source, source.len());
        let path = |segments: &[&str], partial_start| {
            Some(TypePath {
                segments: segments.iter().map(|s| s.to_string()).collect(),
                partial_start,
            })
        };
        assert_eq!(at("struct S {\n    a: types::"), path(&["types"], 25));
        assert_eq!(at("struct S {\n    a: types::Us"), path(&["types"], 25));
        assert_eq!(
            at("struct S {\n    a: package::billing::"),
            path(&["package", "billing"], 36)
        );
        assert_eq!(at("struct S {\n    a: Us"), None);
        assert_eq!(at("struct S {\n    a: "), None);
        assert_eq!(
            at("struct S {\n    a: u8[]::"),
            None,
            "no path before a bracket"
        );
    }
}
