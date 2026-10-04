//! A parsed view of every file the language server can see, and the one
//! name-resolution rule go-to-definition, find-references and rename all
//! share: [`Project::resolve`]. References are then, by construction,
//! exactly the places whose go-to-definition lands on the same declaration.

use std::collections::BTreeSet;
use std::sync::Arc;

use comline_core::schema::idl::grammar::{Declaration, Document, Type};
use lsp_types::{Location, Url};

use crate::analysis::imports::{self, ProjectFile, ResolvedUse};
use crate::analysis::parse_cache;
use crate::analysis::source::{self, DeclaredDependency, ProjectSource};
use crate::analysis::symbols::{self, SymbolTable};
use crate::util::{byte_range_to_lsp_range, word_occurrences};

/// One parsed project file.
pub struct ProjectDoc<'a> {
    pub uri: &'a Url,
    pub source: &'a str,
    pub document: Arc<Document>,
    pub symbols: SymbolTable,
    pub imports: Vec<ResolvedUse>,
    pub namespace: Vec<String>,
    /// The dependency this file belongs to (see [`ProjectSource::dependency`]):
    /// read-only to the package being edited, and not part of it.
    pub dependency: Option<&'a str>,
}

/// Every schema that parses, in the order given. The order matters only for
/// the flat fallback in [`Project::resolve`]: from a given file, the first
/// *other* file (in this order) declaring the name wins.
pub struct Project<'a> {
    pub docs: Vec<ProjectDoc<'a>>,
    /// The namespaces of schemas that were given but don't parse (mid-edit):
    /// they exist, so a `use` of one isn't unresolved, but what they declare
    /// is unknown.
    pub unparsed: Vec<Vec<String>>,
    /// Whether the package's `config.idp` was given - the sign this is the
    /// whole package, not just whatever files happened to be at hand, so an
    /// import nothing here matches really is unresolved.
    pub has_manifest: bool,
    /// What that `config.idp` declares.
    pub dependencies: Vec<DeclaredDependency>,
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
    /// A project over `files`. Schemas are parsed through
    /// [`parse_cache`](crate::analysis::parse_cache); the ones that don't
    /// parse are left out of [`Project::docs`] (their namespaces kept in
    /// [`Project::unparsed`]). A manifest (`config.idp`) among them supplies
    /// [`Project::dependencies`].
    pub fn new<S: ProjectSource + 'a>(files: impl IntoIterator<Item = &'a S>) -> Self {
        Self::build(files.into_iter().map(|file| Input {
            uri: file.uri(),
            text: file.text(),
            namespace: file.namespace(),
            dependency: file.dependency(),
        }))
    }

    /// The active file first, then every other file - the shape every
    /// handler has.
    pub fn with_active<S: ProjectSource>(uri: &'a Url, source: &'a str, other_files: &'a [S]) -> Self {
        let active = Input { uri, text: source, namespace: None, dependency: None };
        Self::build(std::iter::once(active).chain(other_files.iter().map(|file| Input {
            uri: file.uri(),
            text: file.text(),
            namespace: file.namespace(),
            dependency: file.dependency(),
        })))
    }

    /// Like [`Project::new`], from schemas the caller already parsed.
    pub fn from_parsed(files: impl IntoIterator<Item = (&'a Url, &'a str, Arc<Document>)>) -> Self {
        let docs = files
            .into_iter()
            .map(|(uri, source, document)| ProjectDoc::new(uri, source, document, imports::namespace_of(uri), None))
            .collect();

        Self { docs, unparsed: vec![], has_manifest: false, dependencies: vec![] }
    }

    fn build(inputs: impl Iterator<Item = Input<'a>>) -> Self {
        let mut project = Self { docs: vec![], unparsed: vec![], has_manifest: false, dependencies: vec![] };

        for input in inputs {
            if source::is_manifest(input.uri) {
                project.has_manifest = true;
                project.dependencies = source::declared_dependencies(input.text);
                continue;
            }

            let namespace = input.namespace.map(<[String]>::to_vec).unwrap_or_else(|| imports::namespace_of(input.uri));
            match parse_cache::parse(input.uri, input.text) {
                Some(document) => {
                    project.docs.push(ProjectDoc::new(input.uri, input.text, document, namespace, input.dependency))
                }
                None => project.unparsed.push(namespace),
            }
        }

        project
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
    ///    since a miss in step 2 is often just a `use` being typed, or a
    ///    sibling that doesn't parse mid-edit.
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
            // A dependency's own files aren't this package's to search: their
            // `use`s are written against the dependency, not under its name.
            if doc.dependency.is_some() {
                continue;
            }
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

/// One file handed to [`Project::build`].
struct Input<'a> {
    uri: &'a Url,
    text: &'a str,
    namespace: Option<&'a [String]>,
    dependency: Option<&'a str>,
}

impl<'a> ProjectDoc<'a> {
    fn new(
        uri: &'a Url,
        source: &'a str,
        document: Arc<Document>,
        namespace: Vec<String>,
        dependency: Option<&'a str>,
    ) -> Self {
        let symbols = symbols::build_symbol_table(&document, uri, source);
        let imports = imports::resolved_imports(&document, &namespace);
        Self { uri, source, document, symbols, imports, namespace, dependency }
    }

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

/// Every named type written in `doc`'s type positions: its text as written
/// (`Message`, or qualified `types::Message`) and the byte offset each
/// occurrence starts at. A bare name is never matched inside a qualified one.
pub(crate) fn named_type_sites(doc: &ProjectDoc) -> Vec<(String, usize)> {
    let mut sites = BTreeSet::new();

    for (span, ty) in type_spans(&doc.document) {
        let mut names = BTreeSet::new();
        collect_named_text(ty, &mut names);

        let (start, end) = (span.0.min(doc.source.len()), span.1.min(doc.source.len()));
        let text = &doc.source[start..end];
        for name in names {
            for i in word_occurrences(text, &name) {
                if !text[..i].ends_with("::") && !text[i + name.len()..].starts_with("::") {
                    sites.insert((start + i, name.clone()));
                }
            }
        }
    }

    sites.into_iter().map(|(offset, name)| (name, offset)).collect()
}

/// Like [`collect_named`], but the full text as written (`types::User`).
fn collect_named_text(ty: &Type, names: &mut BTreeSet<String>) {
    match ty {
        Type::Named(name) => {
            names.insert(name.text.clone());
        }
        Type::Array(array) => collect_named_text(array.elem_type(), names),
        Type::Union(union) => union.members().iter().for_each(|m| collect_named_text(m, names)),
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
        let project = Project::new(all.iter());
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
        let project = Project::new(all.iter());
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
        let project = Project::new(all.iter());
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
        let project = Project::new(all.iter());
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
        let project = Project::new(all.iter());
        let target = project.resolve(0, "Message").unwrap();

        let refs = project.references(&target);
        assert_eq!(
            lines(&refs),
            vec![("chat.ids".into(), 0, 11)],
            "`m: Message` is the local struct; only the `use` line names `types`'s"
        );
    }
}
