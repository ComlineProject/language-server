//! A parsed view of every file the language server can see, and the one
//! name-resolution rule go-to-definition, find-references and rename all
//! share: [`Project::resolve`]. References are then, by construction,
//! exactly the places whose go-to-definition lands on the same declaration.

use std::collections::BTreeSet;

use comline_core::schema::idl::grammar::{Declaration, Document, Type};
use lsp_types::{Location, Url};

use crate::analysis::imports::{self, ProjectFile, ResolvedUse};
use crate::analysis::symbols::{self, SymbolTable};
use crate::parser;
use crate::util::{byte_range_to_lsp_range, word_occurrences};

/// One parsed project file.
pub struct ProjectDoc<'a> {
    pub uri: &'a Url,
    pub source: &'a str,
    pub document: Document,
    pub symbols: SymbolTable,
    pub imports: Vec<ResolvedUse>,
    namespace: Vec<String>,
}

/// Every file that parses, in the order given. The order matters only for
/// the flat fallback in [`Project::resolve`]: from a given file, the first
/// *other* file (in this order) declaring the name wins.
pub struct Project<'a> {
    pub docs: Vec<ProjectDoc<'a>>,
}

/// A declaration [`Project::resolve`] found: which file (an index into
/// [`Project::docs`]) and the name it's declared under there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub doc: usize,
    pub name: String,
}

/// One place a [`Target`] is referred to.
pub struct Reference {
    pub location: Location,
    /// Whether this occurrence is written as the target's own name, as
    /// opposed to an `as` alias of it - rename edits only these.
    pub spelled_as_target: bool,
}

impl<'a> Project<'a> {
    /// Parse `files` (`(uri, source)` pairs); files that don't parse are
    /// left out.
    pub fn new(files: impl IntoIterator<Item = (&'a Url, &'a str)>) -> Self {
        let docs = files
            .into_iter()
            .filter_map(|(uri, source)| {
                let document = parser::parse(source).ok()?.document?;
                let symbols = symbols::build_symbol_table(&document, uri, source);
                let namespace = imports::namespace_of(uri);
                let imports = imports::resolved_imports(&document, &namespace);
                Some(ProjectDoc { uri, source, document, symbols, imports, namespace })
            })
            .collect();

        Self { docs }
    }

    /// The active file first, then every other file - the shape every
    /// handler has.
    pub fn with_active(uri: &'a Url, source: &'a str, other_files: &'a [(Url, String)]) -> Self {
        Self::new(
            std::iter::once((uri, source))
                .chain(other_files.iter().map(|(u, s)| (u, s.as_str()))),
        )
    }

    pub fn index_of(&self, uri: &Url) -> Option<usize> {
        self.docs.iter().position(|d| d.uri == uri)
    }

    /// What `name`, written in `docs[doc]`, refers to:
    /// 1. a declaration in that same file;
    /// 2. a sibling that one of that file's own `use`/`import`s brings
    ///    `name` into scope from (`imports::resolve_symbol` - so through an
    ///    `as` alias, the target's real name comes back);
    /// 3. failing both, the first other file declaring `name` - a fallback,
    ///    since only open files are visible, so a miss in step 2 is often
    ///    just a sibling that isn't open, or a `use` mid-edit.
    pub fn resolve(&self, doc: usize, name: &str) -> Option<Target> {
        let here = &self.docs[doc];

        if here.symbols.get(name).is_some() {
            return Some(Target { doc, name: name.to_string() });
        }

        let indices: Vec<usize> = self.others(doc).map(|(i, _)| i).collect();
        let files: Vec<ProjectFile> = self.others(doc).map(|(_, d)| d.as_file()).collect();

        if let Some(found) = imports::resolve_symbol(name, &here.imports, &files) {
            let index = indices[files.iter().position(|f| std::ptr::eq(f, found.file))?];
            if self.docs[index].symbols.get(&found.real_name).is_some() {
                return Some(Target { doc: index, name: found.real_name });
            }
        }

        self.others(doc)
            .find(|(_, d)| d.symbols.get(name).is_some())
            .map(|(i, _)| Target { doc: i, name: name.to_string() })
    }

    /// The declaration site of `target`.
    pub fn declaration(&self, target: &Target) -> Option<Location> {
        self.docs[target.doc].symbols.get(&target.name).map(|s| s.location.clone())
    }

    /// Every reference to `target` across the project, declaration
    /// excluded: type positions anywhere that resolve to it (written as
    /// its name or as an `as` alias of it), plus the name as written inside
    /// a `use` line that imports it.
    pub fn references(&self, target: &Target) -> Vec<Reference> {
        let mut found = Vec::new();

        for (index, doc) in self.docs.iter().enumerate() {
            let mut seen = BTreeSet::new();

            for span in type_spans(&doc.document) {
                let mut names = BTreeSet::new();
                collect_named(span.1, &mut names);

                for name in names {
                    if self.resolve(index, &name).as_ref() != Some(target) {
                        continue;
                    }
                    for offset in occurrences_in(doc.source, span.0, &name) {
                        if seen.insert(offset) {
                            found.push(self.reference(doc, offset, &name, name == target.name));
                        }
                    }
                }
            }

            if index != target.doc {
                found.extend(self.use_line_references(index, target));
            }
        }

        found
    }

    /// The target's own name written in a `use` line of `docs[doc]` that
    /// actually imports it from the target's file (`use types::User`,
    /// `use types::{User, Post}`, the `User` in `use types::User as U`).
    fn use_line_references(&self, doc: usize, target: &Target) -> Vec<Reference> {
        let here = &self.docs[doc];
        let siblings: Vec<ProjectFile> = self.others(doc).map(|(_, d)| d.as_file()).collect();
        let target_uri = self.docs[target.doc].uri;

        here.imports
            .iter()
            .filter(|u| {
                imports::resolve_symbol(&target.name, std::slice::from_ref(*u), &siblings)
                    .is_some_and(|r| r.file.uri == target_uri && r.real_name == target.name)
            })
            .flat_map(|u| {
                occurrences_in(here.source, u.span, &target.name)
                    .into_iter()
                    .filter(move |&offset| !follows_as(here.source, u.span.0, offset))
            })
            .map(|offset| self.reference(here, offset, &target.name, true))
            .collect()
    }

    fn reference(&self, doc: &ProjectDoc, offset: usize, name: &str, spelled_as_target: bool) -> Reference {
        Reference {
            location: Location {
                uri: doc.uri.clone(),
                range: byte_range_to_lsp_range(doc.source, offset, offset + name.len()),
            },
            spelled_as_target,
        }
    }

    fn others(&self, doc: usize) -> impl Iterator<Item = (usize, &ProjectDoc<'a>)> {
        self.docs.iter().enumerate().filter(move |(i, _)| *i != doc)
    }
}

impl<'a> ProjectDoc<'a> {
    fn as_file(&self) -> ProjectFile<'a> {
        ProjectFile { uri: self.uri, source: self.source, namespace: self.namespace.clone() }
    }
}

/// Whole-word occurrences of `name` inside `span` that name a symbol, not a
/// namespace segment - i.e. not immediately followed by `::` (in
/// `use User::User`, only the second one).
fn occurrences_in(source: &str, span: (usize, usize), name: &str) -> Vec<usize> {
    let (start, end) = (span.0.min(source.len()), span.1.min(source.len()));
    let text = &source[start..end];

    word_occurrences(text, name)
        .into_iter()
        .filter(|&i| !text[i + name.len()..].starts_with("::"))
        .map(|i| start + i)
        .collect()
}

/// Whether the word at `offset` is the alias in `... as Alias`, looking
/// back no further than `from`.
fn follows_as(source: &str, from: usize, offset: usize) -> bool {
    source[from..offset].trim_end().strip_suffix("as").is_some_and(|before| {
        before.ends_with(|c: char| c.is_whitespace())
    })
}

/// Every type position in a document, with its byte span.
fn type_spans(document: &Document) -> Vec<((usize, usize), &Type)> {
    let mut spans = Vec::new();

    for decl in &document.0 {
        match &**decl {
            Declaration::Struct(s) => {
                spans.extend(s.fields().iter().map(|f| (f.field_type_span(), f.field_type())));
            }
            Declaration::Error(e) => {
                spans.extend(e.fields().iter().map(|f| (f.field_type_span(), f.field_type())));
            }
            Declaration::Validator(v) => {
                spans.extend(v.properties.iter().map(|p| (p.property_type.span, &p.property_type.value)));
            }
            Declaration::Protocol(p) => {
                for func in p.functions() {
                    if let Some(args) = func.args() {
                        spans.push((args.first().arg_type_span(), args.first().arg_type()));
                        for arg in args.rest() {
                            let arg = arg.arg_type();
                            spans.push((arg.arg_type_span(), arg.arg_type()));
                        }
                    }
                    if let Some(ret) = func.return_type() {
                        spans.push((ret.return_type_span(), ret.return_type()));
                    }
                }
            }
            Declaration::Const(c) => spans.push((c.type_def_span(), c.type_def())),
            Declaration::TypeAlias(t) => spans.push((t.target_type_span(), t.target_type())),
            Declaration::Import(_)
            | Declaration::Use(_)
            | Declaration::Enum(_)
            | Declaration::Settings(_) => {}
        }
    }

    spans
}

/// The symbol names a type refers to - the last segment of each named type
/// (`types::User` names `User`), through arrays and unions.
fn collect_named(ty: &Type, names: &mut BTreeSet<String>) {
    match ty {
        Type::Named(name) => {
            if let Some(last) = name.text.rsplit("::").next() {
                names.insert(last.to_string());
            }
        }
        Type::Array(array) => collect_named(array.elem_type(), names),
        Type::Union(union) => union.members().iter().for_each(|m| collect_named(m, names)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<(Url, String)> {
        list.iter().map(|(u, s)| (Url::parse(u).unwrap(), s.to_string())).collect()
    }

    fn lines(refs: &[Reference]) -> Vec<(String, u32, u32)> {
        let mut out: Vec<_> = refs
            .iter()
            .map(|r| {
                let file = r.location.uri.path().rsplit('/').next().unwrap().to_string();
                (file, r.location.range.start.line, r.location.range.start.character)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn every_reference_has_its_own_position() {
        // The old handler found each reference with `source.find(name)` -
        // every one of them landed on the first `User` in the file.
        let all = files(&[(
            "file:///a.ids",
            "struct User {\n    name: string\n}\n\nstruct A {\n    u: User\n}\n\nstruct B {\n    list: User[]\n}\n",
        )]);
        let project = Project::new(all.iter().map(|(u, s)| (u, s.as_str())));
        let target = project.resolve(0, "User").unwrap();

        let refs = project.references(&target);
        assert_eq!(lines(&refs), vec![("a.ids".into(), 5, 7), ("a.ids".into(), 9, 10)]);
    }

    #[test]
    fn references_follow_use_scoping_across_files() {
        let all = files(&[
            ("file:///types.ids", "struct Message {\n    text: string\n}\n"),
            ("file:///other.ids", "struct Message {\n    other: bool\n}\n"),
            ("file:///chat.ids", "use types::Message\n\nprotocol Chat {\n    function send(Message) -> Message;\n}\n"),
            ("file:///unrelated.ids", "use other::Message\n\nstruct S {\n    m: Message\n}\n"),
        ]);
        let project = Project::new(all.iter().map(|(u, s)| (u, s.as_str())));
        let target = project.resolve(0, "Message").unwrap();

        let refs = project.references(&target);
        assert_eq!(
            lines(&refs),
            vec![("chat.ids".into(), 0, 11), ("chat.ids".into(), 3, 18), ("chat.ids".into(), 3, 30)],
            "only `chat.ids` imports `types::Message`; `unrelated.ids` imports `other`'s"
        );
    }

    #[test]
    fn an_alias_counts_as_a_reference_but_is_not_spelled_as_the_target() {
        let all = files(&[
            ("file:///types.ids", "struct Message {\n    text: string\n}\n"),
            ("file:///chat.ids", "use types::Message as Msg\n\nstruct S {\n    m: Msg\n}\n"),
        ]);
        let project = Project::new(all.iter().map(|(u, s)| (u, s.as_str())));
        let target = project.resolve(1, "Msg").unwrap();
        assert_eq!(target, Target { doc: 0, name: "Message".into() });

        let refs = project.references(&target);
        let mut spelled: Vec<_> = refs
            .iter()
            .map(|r| (r.location.range.start.line, r.spelled_as_target))
            .collect();
        spelled.sort();
        assert_eq!(spelled, vec![(0, true), (3, false)], "the `use` line's `Message`, then `m: Msg`");
    }

    #[test]
    fn a_namespace_segment_with_the_same_name_is_not_a_reference() {
        let all = files(&[
            ("file:///pkg/src/User.ids", "struct User {\n    name: string\n}\n"),
            ("file:///pkg/src/app.ids", "use User::User\n\nstruct S {\n    u: User\n}\n"),
        ]);
        let project = Project::new(all.iter().map(|(u, s)| (u, s.as_str())));
        let target = project.resolve(0, "User").unwrap();

        let refs = project.references(&target);
        assert_eq!(
            lines(&refs),
            vec![("app.ids".into(), 0, 10), ("app.ids".into(), 3, 7)],
            "the namespace `User::` at column 4 is not the struct"
        );
    }

    #[test]
    fn a_local_declaration_shadows_an_import() {
        let all = files(&[
            ("file:///types.ids", "struct Message {\n    text: string\n}\n"),
            ("file:///chat.ids", "use types::Message\n\nstruct Message {\n    local: bool\n}\n\nstruct S {\n    m: Message\n}\n"),
        ]);
        let project = Project::new(all.iter().map(|(u, s)| (u, s.as_str())));
        let target = project.resolve(0, "Message").unwrap();

        let refs = project.references(&target);
        assert_eq!(
            lines(&refs),
            vec![("chat.ids".into(), 0, 11)],
            "`m: Message` is the local struct; only the `use` line names `types`'s"
        );
    }
}
