//! Which type names a file's `use`s bring into scope, given the package's
//! other files - and which names it uses that are declared in another file
//! but never imported (a missing `use`).
//!
//! In the editor, core validates one file at a time, without the project,
//! so it can only expand a `use` it can read on its own: `use types::User`
//! and `use types::User as U` work, but `use types::*`, `use types::{A, B}`
//! and `use types` bring in nothing, and every name from them was reported
//! as an unknown type. [`check`] does what core does with the whole project
//! (`resolve_use_declaration`: expand a glob or whole-namespace `use` into
//! every name the target declares, an item list into its items), with the
//! files it's given standing in for the project, and hands the result back as
//! extra `FrozenUnit::Import`s for core's own validator ([`ImportCheck::scope`]).
//!
//! Where the target of a glob or whole-namespace `use` isn't among those
//! files (a dependency package, `std::`, or a file the caller didn't pass),
//! there is no way to know what it declares, so names that could come from
//! it are given the benefit of the doubt - `comline build` sees everything
//! and still catches a real mistake there; the editor shouldn't invent one.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use comline_core::schema::idl::grammar::Declaration;
use comline_core::schema::ir::compiler::import_resolver::ImportResolver;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use lsp_types::SymbolKind;

use crate::analysis::imports::ResolvedUse;
use crate::analysis::project::{named_type_sites, Project, ProjectDoc};
use crate::analysis::source::DependencyKind;
use crate::analysis::stdlib;
use crate::util::{closest, word_occurrences};

/// What [`check`] found for one file.
pub struct ImportCheck {
    /// Imports core's per-file lowering can't produce on its own, to append
    /// to the file's units before validation.
    pub scope: Vec<FrozenUnit>,
    /// Each use of a bare type name that's declared in another project file
    /// but not brought into scope by any `use` here.
    pub missing: Vec<MissingImport>,
    /// Each `use` that doesn't resolve - `comline check` / `build` reject it.
    pub unresolved: Vec<UnresolvedImport>,
    /// Each `use` of a declared dependency there are no files for, so it
    /// can't be checked here.
    pub unverified: Vec<UnverifiedImport>,
}

/// A `use` that doesn't resolve, the way core's `check_imports` sees it.
pub struct UnresolvedImport {
    /// Byte range to underline: the segment or item that doesn't resolve
    /// when it can be pinned down, else the whole `use`.
    pub range: (usize, usize),
    /// Core's wording: `no schema in this package or its dependencies
    /// matches 'typse::User'`, `schema 'types' doesn't declare 'Nope'`.
    pub detail: String,
    /// A close name that could replace `range` (the "did you mean").
    pub suggestion: Option<String>,
}

/// A `use` of a declared dependency there are no files for.
pub struct UnverifiedImport {
    pub range: (usize, usize),
    /// Why there are no files: not fetched yet, a registry source, ...
    pub reason: String,
}

/// One occurrence of a type name that needs a `use`.
pub struct MissingImport {
    pub name: String,
    /// Byte range of this occurrence.
    pub range: (usize, usize),
    /// Every project file declaring `name` as a type, in project order.
    pub candidates: Vec<Candidate>,
    /// The alias a `use` here already imports `name` under, if any - core
    /// binds only the alias (`use types::User as U` makes `U` usable, not
    /// `User`).
    pub imported_as: Option<String>,
}

/// A project file a missing name could be imported from.
pub struct Candidate {
    pub namespace: Vec<String>,
    /// The file's name, for messages (`types.ids`).
    pub file: String,
}

impl Candidate {
    /// The path a `use` for `name` from this file would take: `types::User`.
    pub fn use_path(&self, name: &str) -> String {
        qualified(&self.namespace, name)
    }
}

/// Where one `use` points, as far as the project's files can tell.
pub(crate) enum Target {
    /// A project file (index into `Project::docs`), with the rest of the path
    /// past its namespace (`["User"]` for `use types::User`, empty for
    /// `use types`, `use types::*` or `use types::{A, B}`).
    Found(usize, Vec<String>),
    /// No project file has the namespace (or any prefix of it) - a
    /// dependency package, `std::`, or a file the caller didn't pass.
    Outside,
}

pub fn check(project: &Project, doc: usize) -> ImportCheck {
    let here = &project.docs[doc];
    let sites = named_type_sites(here);
    let bare_sites: BTreeSet<&str> = sites
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !name.contains("::") && here.symbols.get(name).is_none())
        .collect();

    let mut scope = Vec::new();
    // Bare names some `use` here brings (or may bring) into scope.
    let mut covered: BTreeSet<String> = BTreeSet::new();
    // Symbols imported under an alias: real name -> alias.
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();

    for use_decl in &here.imports {
        let resolved = &use_decl.resolved;
        let namespace = &resolved.absolute_namespace;
        let glob = resolved.symbols == ["*"];
        let items = !glob && !resolved.symbols.is_empty();

        match target_of(project, Some(doc), use_decl) {
            Target::Found(sibling, remaining) => {
                let sibling = &project.docs[sibling];
                if glob || (!items && remaining.is_empty()) {
                    // Glob or whole namespace: everything the file declares.
                    for name in declared_names(sibling) {
                        scope.push(import(&sibling.namespace, &name, use_decl.span));
                        covered.insert(name);
                    }
                } else if items {
                    for item in &resolved.symbols {
                        scope.push(import(&sibling.namespace, item, use_decl.span));
                        covered.insert(item.clone());
                    }
                } else {
                    // A single symbol - core already registers this one.
                    covered.insert(single_symbol_name(use_decl, &remaining, &mut aliases));
                }
            }
            Target::Outside => {
                if items {
                    // Known without the file: the items are listed right here.
                    for item in &resolved.symbols {
                        scope.push(import(namespace, item, use_decl.span));
                        covered.insert(item.clone());
                    }
                } else if glob || namespace.len() == 1 {
                    // A glob, or a whole namespace (a one-segment path can't
                    // name a symbol - every schema has a namespace): any name
                    // used here may come from it.
                    for name in &bare_sites {
                        scope.push(import(namespace, name, use_decl.span));
                        covered.insert(name.to_string());
                    }
                    // Qualified uses of that namespace (`types::User`).
                    for (text, _) in &sites {
                        if text.strip_prefix(&qualified(namespace, "")).is_some_and(|rest| !rest.contains("::")) {
                            scope.push(FrozenUnit::Import(text.clone(), None, use_decl.span));
                        }
                    }
                } else {
                    // `use a::b`: read as the symbol `b` (or its alias),
                    // which core already registers.
                    covered.insert(single_symbol_name(use_decl, &namespace[namespace.len() - 1..], &mut aliases));
                }
            }
        }
    }

    let mut missing = Vec::new();
    for (name, offset) in &sites {
        if name.contains("::") || here.symbols.get(name).is_some() || covered.contains(name) {
            continue;
        }

        let candidates: Vec<Candidate> = project
            .docs
            .iter()
            .enumerate()
            .filter(|(i, d)| *i != doc && declares_type(d, name))
            .map(|(_, d)| Candidate { namespace: d.namespace.clone(), file: file_name(d) })
            .collect();

        if let Some(first) = candidates.first() {
            // Core would report this as a bare "Unknown type"; the missing
            // `use` is reported instead, with where the type actually is.
            scope.push(import(&first.namespace, name, (*offset, offset + name.len())));
            missing.push(MissingImport {
                name: name.clone(),
                range: (*offset, offset + name.len()),
                candidates,
                imported_as: aliases.get(name).cloned(),
            });
        }
    }

    let (unresolved, unverified) = resolution(project, doc);
    ImportCheck { scope, missing, unresolved, unverified }
}

/// Every `use` in `docs[doc]` that doesn't resolve, by core's own rule
/// (`check_imports`): a path no schema of the project matches, or an item
/// the matched schema doesn't declare. `std::` paths aren't checked (std
/// isn't part of a build yet), and two cases get the benefit of the doubt:
/// a schema that exists but doesn't parse right now, and a declared
/// dependency the project has no files for (reported as unverified, with the
/// reason).
///
/// Only with the package's `config.idp` in the project: without it, the
/// files at hand may not be the whole package (a single file in the
/// playground), and nothing could be called unresolved.
fn resolution(project: &Project, doc: usize) -> (Vec<UnresolvedImport>, Vec<UnverifiedImport>) {
    if !project.has_manifest {
        return (vec![], vec![]);
    }
    let here = &project.docs[doc];
    let resolver = ImportResolver::new(vec![], HashMap::new(), None);

    let top_level: BTreeSet<&str> = project
        .docs
        .iter()
        .map(|d| &d.namespace)
        .chain(&project.unparsed)
        .filter_map(|namespace| namespace.first().map(String::as_str))
        .collect();
    let declared: BTreeMap<&str, &DependencyKind> =
        project.dependencies.iter().map(|d| (d.name.as_str(), &d.kind)).collect();
    let indexed: BTreeSet<&str> = project.docs.iter().filter_map(|d| d.dependency).collect();

    let mut unresolved = Vec::new();
    let mut unverified = Vec::new();

    for decl in &here.document.0 {
        let Declaration::Use(use_stmt) = &decl.value else {
            continue;
        };
        let span = decl.span;
        let text = &here.source[span.0.min(here.source.len())..span.1.min(here.source.len())];

        let resolved = match resolver.resolve_namespace(&use_stmt.path, &here.namespace) {
            Ok(resolved) => resolved,
            Err(detail) => {
                unresolved.push(UnresolvedImport { range: span, detail, suggestion: None });
                continue;
            }
        };
        let namespace = resolved.absolute_namespace.clone();
        let first = namespace.first().map(String::as_str).unwrap_or_default();
        let use_decl = ResolvedUse {
            resolved,
            alias: use_stmt.alias.as_ref().map(|a| a.name.text.clone()),
            span,
        };

        match target_of(project, Some(doc), &use_decl) {
            Target::Found(sibling, remaining) => {
                let target = &project.docs[sibling];
                let symbols = &use_decl.resolved.symbols;
                let named: Vec<String> = if symbols == &["*"] || (symbols.is_empty() && remaining.is_empty()) {
                    vec![]
                } else if !symbols.is_empty() {
                    symbols.clone()
                } else {
                    vec![remaining.join("::")]
                };

                let importable = importable_names(target);
                for name in named.iter().filter(|name| !importable.contains(*name)) {
                    let item = name.rsplit("::").next().unwrap_or(name);
                    let range = find_in(text, span.0, item, false);
                    unresolved.push(UnresolvedImport {
                        range: range.unwrap_or(span),
                        detail: format!("schema '{}' doesn't declare '{name}'", target.namespace.join("::")),
                        suggestion: range.and(closest(item, importable.iter().map(String::as_str))),
                    });
                }
            }
            Target::Outside => {
                // A schema that exists but doesn't parse right now.
                if project.unparsed.iter().any(|u| !u.is_empty() && namespace.starts_with(u)) {
                    continue;
                }

                if let Some(kind) = declared.get(first) {
                    if !indexed.contains(first) {
                        unverified.push(UnverifiedImport { range: span, reason: unverified_reason(first, kind) });
                        continue;
                    }
                }

                if first == stdlib::NAME {
                    unresolved.push(unknown_std_path(project, text, span, &namespace));
                    continue;
                }

                let known = top_level.contains(first) || declared.contains_key(first);
                let range = if known { None } else { find_in(text, span.0, first, true) };
                unresolved.push(UnresolvedImport {
                    range: range.unwrap_or(span),
                    detail: format!(
                        "no schema in this package or its dependencies matches '{}'",
                        namespace.join("::")
                    ),
                    suggestion: range.and(closest(first, top_level.iter().copied().chain(declared.keys().copied()))),
                });
            }
        }
    }

    (unresolved, unverified)
}

/// A `std::` path no std schema matches, worded like the build's, with the
/// closest std namespace (`std::htp` → `std::http`) as the quick fix for the
/// two segments written.
fn unknown_std_path(project: &Project, text: &str, span: (usize, usize), namespace: &[String]) -> UnresolvedImport {
    let written = namespace.iter().take(2).cloned().collect::<Vec<_>>().join("::");
    let std_namespaces: BTreeSet<String> = project
        .docs
        .iter()
        .filter(|d| d.dependency == Some(stdlib::NAME))
        .map(|d| d.namespace.iter().take(2).cloned().collect::<Vec<_>>().join("::"))
        .collect();
    let range = text.find(&written).map(|i| (span.0 + i, span.0 + i + written.len()));
    UnresolvedImport {
        range: range.unwrap_or(span),
        detail: format!("std has no schema matching '{}'", namespace.join("::")),
        suggestion: range.and(closest(&written, std_namespaces.iter().map(String::as_str))),
    }
}

fn unverified_reason(name: &str, kind: &DependencyKind) -> String {
    match kind {
        DependencyKind::Git => format!("dependency `{name}` isn't fetched yet — run `comline check` to fetch it"),
        DependencyKind::Registry => {
            format!("dependency `{name}` comes from a registry, which isn't supported yet")
        }
        DependencyKind::Path(path) => format!("dependency `{name}` has no schemas at `{path}`"),
    }
}

/// The byte range of `word` in a `use` line's `text` (starting at `base`):
/// a namespace segment (followed by `::`) or an item (not followed by `::`,
/// and not the alias after `as`).
fn find_in(text: &str, base: usize, word: &str, segment: bool) -> Option<(usize, usize)> {
    word_occurrences(text, word)
        .into_iter()
        .find(|&i| {
            let followed_by_path = text[i + word.len()..].starts_with("::");
            let after_as = text[..i].trim_end().strip_suffix("as").is_some_and(|before| before.ends_with(char::is_whitespace));
            followed_by_path == segment && !after_as
        })
        .map(|i| (base + i, base + i + word.len()))
}

/// Every name a `use` can import from a schema: its types, protocols and
/// consts, plus errors (`! Name` throws), validators and settings - what
/// core's `check_imports` accepts.
fn importable_names(doc: &ProjectDoc) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = doc.symbols.all_symbols().into_iter().map(|s| s.name.clone()).collect();
    names.extend(doc.document.0.iter().filter_map(|decl| match &decl.value {
        Declaration::Error(e) => Some(e.name.text.clone()),
        Declaration::Validator(v) => Some(v.name.text.clone()),
        Declaration::Settings(s) => Some(s.name.text.clone()),
        _ => None,
    }));
    names
}

/// The project file a `use` points at: the longest prefix of its namespace
/// that some project file other than `exclude` (the file the `use` is in) has.
/// A deeper file always wins: `use chat::admin::X` means `chat/admin.ids`
/// when it's open, not `chat.ids`. The same search as
/// [`crate::analysis::imports::resolve_symbol`].
pub(crate) fn target_of(project: &Project, exclude: Option<usize>, use_decl: &ResolvedUse) -> Target {
    let namespace = &use_decl.resolved.absolute_namespace;

    for split_at in (1..=namespace.len()).rev() {
        let prefix = &namespace[..split_at];
        if let Some(i) = (0..project.docs.len()).find(|&i| Some(i) != exclude && project.docs[i].namespace == prefix) {
            return Target::Found(i, namespace[split_at..].to_vec());
        }
    }

    Target::Outside
}

/// The name a single-symbol `use` binds, the way core's validator registers
/// it: the alias if there is one (recorded in `aliases`), else the symbol.
fn single_symbol_name(use_decl: &ResolvedUse, remaining: &[String], aliases: &mut BTreeMap<String, String>) -> String {
    let symbol = remaining.join("::");
    match &use_decl.alias {
        Some(alias) => {
            aliases.insert(symbol, alias.clone());
            alias.clone()
        }
        None => symbol,
    }
}

/// Every name a file declares, the way core expands a glob or
/// whole-namespace `use` (`declared_symbol_names`).
fn declared_names(doc: &ProjectDoc) -> Vec<String> {
    doc.symbols.all_symbols().into_iter().map(|s| s.name.clone()).collect()
}

/// Whether `doc` declares `name` as something usable in a type position.
pub(crate) fn declares_type(doc: &ProjectDoc, name: &str) -> bool {
    doc.symbols.get(name).is_some_and(|s| is_type_kind(s.kind))
}

/// Struct, enum and type alias - not protocols or consts.
pub(crate) fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::STRUCT | SymbolKind::ENUM | SymbolKind::TYPE_PARAMETER)
}

pub(crate) fn file_name(doc: &ProjectDoc) -> String {
    std::path::Path::new(doc.uri.path())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| doc.uri.to_string())
}

fn qualified(namespace: &[String], name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", namespace.join("::"), name)
    }
}

fn import(namespace: &[String], name: &str, span: (usize, usize)) -> FrozenUnit {
    FrozenUnit::Import(qualified(namespace, name), None, span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Url;

    fn project(files: &[(&str, &str)]) -> Vec<(Url, String)> {
        files.iter().map(|(u, s)| (Url::parse(u).unwrap(), s.to_string())).collect()
    }

    fn missing_names(files: &[(Url, String)], doc: usize) -> Vec<(String, Vec<String>)> {
        let project = Project::new(files.iter());
        let doc = project.index_of(&files[doc].0).unwrap();
        check(&project, doc)
            .missing
            .into_iter()
            .map(|m| {
                let paths = m.candidates.iter().map(|c| c.use_path(&m.name)).collect();
                (m.name, paths)
            })
            .collect()
    }

    const TYPES: (&str, &str) = ("file:///pkg/src/types.ids", "struct Message {\n    text: string\n}\n\nenum Kind {\n    A\n}\n");

    #[test]
    fn a_type_from_another_file_without_a_use_is_missing() {
        let files = project(&[("file:///pkg/src/chat.ids", "struct S {\n    m: Message\n}\n"), TYPES]);
        assert_eq!(missing_names(&files, 0), vec![("Message".into(), vec!["types::Message".into()])]);
    }

    #[test]
    fn every_use_form_that_imports_the_name_counts() {
        for header in [
            "use types::Message",
            "use types::*",
            "use types::{Message, Kind}",
            "use types",
            "use package::types::Message",
            "import types::Message",
        ] {
            let source = format!("{header}\n\nstruct S {{\n    m: Message\n}}\n");
            let files = project(&[("file:///pkg/src/chat.ids", &source), TYPES]);
            assert_eq!(missing_names(&files, 0), vec![], "`{header}` imports `Message`");
        }
    }

    #[test]
    fn a_bare_prefix_before_a_glob_or_list_imports_the_name() {
        // From `types/inner.ids`, `parent::` is `types`.
        for header in ["use parent::*", "use parent::{Message, Kind}"] {
            let source = format!("{header}\n\nstruct S {{\n    m: Message\n}}\n");
            let files = project(&[("file:///pkg/src/types/inner.ids", &source), TYPES]);
            assert_eq!(missing_names(&files, 0), vec![], "`{header}` imports `Message`");
        }

        let source = "use parent::{Kind}\n\nstruct S {\n    m: Message\n}\n";
        let files = project(&[("file:///pkg/src/types/inner.ids", source), TYPES]);
        let missing = missing_names(&files, 0);
        assert_eq!(missing.len(), 1, "a list that leaves `Message` out doesn't import it: {missing:?}");
        assert_eq!(missing[0].0, "Message");
    }

    #[test]
    fn an_alias_binds_only_the_alias() {
        let aliased = "use types::Message as Msg\n\nstruct S {\n    a: Msg\n    b: Message\n}\n";
        let files = project(&[("file:///pkg/src/chat.ids", aliased), TYPES]);
        let project = Project::new(files.iter());

        let missing = check(&project, 0).missing;
        assert_eq!(missing.len(), 1, "`Msg` is fine; `Message` isn't bound");
        assert_eq!(missing[0].name, "Message");
        assert_eq!(missing[0].imported_as.as_deref(), Some("Msg"));
    }

    #[test]
    fn a_use_of_something_else_from_the_same_file_does_not_count() {
        for header in ["use types::Kind", "use types::{Kind}"] {
            let source = format!("{header}\n\nstruct S {{\n    m: Message\n}}\n");
            let files = project(&[("file:///pkg/src/chat.ids", &source), TYPES]);
            assert_eq!(missing_names(&files, 0).len(), 1, "`{header}` doesn't import `Message`");
        }
    }

    #[test]
    fn a_glob_or_whole_namespace_use_of_a_file_outside_the_project_gets_the_benefit_of_the_doubt() {
        for header in ["use other::*", "use other"] {
            let source = format!("{header}\n\nstruct S {{\n    m: Message\n}}\n");
            let files = project(&[("file:///pkg/src/chat.ids", &source), TYPES]);
            assert_eq!(missing_names(&files, 0), vec![], "`Message` may come from `{header}`");
        }
    }

    #[test]
    fn a_glob_of_a_project_file_that_doesnt_declare_the_name_does_not_count() {
        let files = project(&[
            ("file:///pkg/src/chat.ids", "use other::*\n\nstruct S {\n    m: Message\n}\n"),
            ("file:///pkg/src/other.ids", "struct Unrelated {\n    x: bool\n}\n"),
            TYPES,
        ]);
        assert_eq!(missing_names(&files, 0), vec![("Message".into(), vec!["types::Message".into()])]);
    }

    #[test]
    fn a_local_declaration_needs_no_import() {
        let files = project(&[
            ("file:///pkg/src/chat.ids", "struct Message {\n    x: bool\n}\n\nstruct S {\n    m: Message\n}\n"),
            TYPES,
        ]);
        assert_eq!(missing_names(&files, 0), vec![]);
    }

    #[test]
    fn every_open_file_declaring_the_name_is_a_candidate() {
        let files = project(&[
            ("file:///pkg/src/chat.ids", "struct S {\n    m: Message\n    list: Message[]\n}\n"),
            TYPES,
            ("file:///pkg/src/legacy/types.ids", "struct Message {\n    old: bool\n}\n"),
        ]);
        let found = missing_names(&files, 0);
        assert_eq!(found.len(), 2, "one per occurrence: {found:?}");
        assert_eq!(found[0].1, vec!["types::Message".to_string(), "legacy::types::Message".to_string()]);
    }

    #[test]
    fn a_protocol_or_const_is_not_offered_as_a_type() {
        let files = project(&[
            ("file:///pkg/src/chat.ids", "struct S {\n    m: Service\n}\n"),
            ("file:///pkg/src/api.ids", "protocol Service {\n    function f();\n}\n"),
        ]);
        assert_eq!(missing_names(&files, 0), vec![]);
    }

    // --- Resolution, with the package's manifest in the view ---

    use crate::analysis::source::SourceFile;

    const MANIFEST: &str = "congregation app\nspecification_version = 1\n\n\
        dependencies = {\n    \
            shared = {\n        path = \"../shared\"\n    }\n    \
            net = {\n        version = \"1.0.0\"\n        uri = \"https://example.test/net\"\n        commit = \"abc\"\n    }\n\
        }\n";

    /// `chat.ids` (the file checked) with `body`, plus `types.ids`, the
    /// manifest, and the `shared` dependency's `models.ids`.
    /// (underlined text, detail, suggestion) per unresolved import.
    type Found = Vec<(String, String, Option<String>)>;

    fn resolution_of(body: &str) -> (Found, Vec<String>) {
        let mut files = vec![
            SourceFile::local(Url::parse("file:///pkg/src/chat.ids").unwrap(), body.to_string()),
            SourceFile::local(
                Url::parse("file:///pkg/src/types.ids").unwrap(),
                "struct User {\n    id: u64\n}\n\nerror Gone {\n    message = \"gone\"\n}\n".to_string(),
            ),
            SourceFile::local(Url::parse("file:///pkg/src/broken.ids").unwrap(), "struct {".to_string()),
            SourceFile::local(Url::parse("file:///pkg/config.idp").unwrap(), MANIFEST.to_string()),
            SourceFile::of_dependency(
                Url::parse("file:///shared/src/models.ids").unwrap(),
                "struct Thing {\n    id: u64\n}\n".to_string(),
                "shared",
                vec!["shared".to_string(), "models".to_string()],
            ),
        ];
        files.extend(crate::analysis::stdlib::files(&crate::analysis::stdlib::root()));
        let project = Project::new(files.iter());
        let check = check(&project, 0);
        let unresolved = check
            .unresolved
            .iter()
            .map(|u| (body[u.range.0..u.range.1].to_string(), u.detail.clone(), u.suggestion.clone()))
            .collect();
        (unresolved, check.unverified.iter().map(|u| u.reason.clone()).collect())
    }

    #[test]
    fn a_typo_in_the_namespace_is_unresolved_with_a_suggestion() {
        let (unresolved, _) = resolution_of("use typse::User\n\nstruct S {\n    u: User\n}\n");
        assert_eq!(
            unresolved,
            vec![(
                "typse".to_string(),
                "no schema in this package or its dependencies matches 'typse::User'".to_string(),
                Some("types".to_string())
            )]
        );
    }

    #[test]
    fn an_item_the_schema_does_not_declare_is_unresolved() {
        let (unresolved, _) = resolution_of("use types::{User, Usr}\n\nstruct S {\n    u: User\n}\n");
        assert_eq!(
            unresolved,
            vec![("Usr".to_string(), "schema 'types' doesn't declare 'Usr'".to_string(), Some("User".to_string()))]
        );
    }

    #[test]
    fn imports_that_resolve_or_cannot_be_checked_are_not_unresolved() {
        for body in [
            "use types::User\n\nstruct S {\n    u: User\n}\n",
            "use types::Gone\n\nstruct S {\n    id: u64\n}\n",
            "use types\n\nstruct S {\n    u: User\n}\n",
            "use std::http::{Request, Response}\n\nstruct S {\n    r: Request\n}\n",
            "use std::validators::StringBounds\n\nstruct S {\n    id: u64\n}\n",
            "use broken::Anything\n\nstruct S {\n    id: u64\n}\n",
            "use shared::models::Thing\n\nstruct S {\n    t: Thing\n}\n",
        ] {
            let (unresolved, unverified) = resolution_of(body);
            assert!(unresolved.is_empty() && unverified.is_empty(), "{body}: {unresolved:?} {unverified:?}");
        }
    }

    #[test]
    fn std_is_checked_like_the_build_checks_it() {
        let (unresolved, _) = resolution_of("use std::htp::Request\n\nstruct S {\n    id: u64\n}\n");
        assert_eq!(
            unresolved,
            vec![(
                "std::htp".to_string(),
                "std has no schema matching 'std::htp::Request'".to_string(),
                Some("std::http".to_string())
            )]
        );

        let (unresolved, _) = resolution_of("use std::http::Reqest\n\nstruct S {\n    id: u64\n}\n");
        assert_eq!(
            unresolved,
            vec![(
                "Reqest".to_string(),
                "schema 'std::http' doesn't declare 'Reqest'".to_string(),
                Some("Request".to_string())
            )]
        );
    }

    #[test]
    fn a_dependency_with_files_is_checked_like_the_package() {
        let (unresolved, _) = resolution_of("use shared::models::Thign\n\nstruct S {\n    id: u64\n}\n");
        assert_eq!(
            unresolved,
            vec![(
                "Thign".to_string(),
                "schema 'shared::models' doesn't declare 'Thign'".to_string(),
                Some("Thing".to_string())
            )]
        );

        let (unresolved, _) = resolution_of("use shared::nothing::X\n\nstruct S {\n    id: u64\n}\n");
        assert_eq!(unresolved.len(), 1, "{unresolved:?}");
        assert_eq!(unresolved[0].1, "no schema in this package or its dependencies matches 'shared::nothing::X'");
    }

    #[test]
    fn a_dependency_without_files_is_not_checked_and_says_why() {
        let (unresolved, unverified) = resolution_of("use net::wire::Frame\n\nstruct S {\n    id: u64\n}\n");
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(unverified, vec!["dependency `net` isn't fetched yet — run `comline check` to fetch it".to_string()]);
    }

    #[test]
    fn without_the_manifest_nothing_is_called_unresolved() {
        let files = project(&[("file:///pkg/src/chat.ids", "use typse::User\n\nstruct S {\n    id: u64\n}\n")]);
        let project = Project::new(files.iter());
        assert!(check(&project, 0).unresolved.is_empty());
    }

    #[test]
    fn a_type_from_a_dependency_is_a_missing_import_candidate() {
        let files = [
            SourceFile::local(Url::parse("file:///pkg/src/chat.ids").unwrap(), "struct S {\n    t: Thing\n}\n".to_string()),
            SourceFile::local(Url::parse("file:///pkg/config.idp").unwrap(), MANIFEST.to_string()),
            SourceFile::of_dependency(
                Url::parse("file:///shared/src/models.ids").unwrap(),
                "struct Thing {\n    id: u64\n}\n".to_string(),
                "shared",
                vec!["shared".to_string(), "models".to_string()],
            ),
        ];
        let project = Project::new(files.iter());
        let missing = check(&project, 0).missing;
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].candidates[0].use_path("Thing"), "shared::models::Thing");
    }
}

