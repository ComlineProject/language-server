//! Which type names a file's `use`s bring into scope, given the other open
//! files - and which names it uses that are declared in another open file
//! but never imported (a missing `use`).
//!
//! In the editor, core validates one file at a time, without the project,
//! so it can only expand a `use` it can read on its own: `use types::User`
//! and `use types::User as U` work, but `use types::*`, `use types::{A, B}`
//! and `use types` bring in nothing, and every name from them was reported
//! as an unknown type. [`check`] does what core does with the whole project
//! (`resolve_use_declaration`: expand a glob or whole-namespace `use` into
//! every name the target declares, an item list into its items), with the
//! open files standing in for the project, and hands the result back as
//! extra `FrozenUnit::Import`s for core's own validator ([`ImportCheck::scope`]).
//!
//! Where the target of a glob or whole-namespace `use` isn't open, there is
//! no way to know what it declares, so names that could come from it are
//! given the benefit of the doubt - `comline build` sees every file and
//! still catches a real mistake there; the editor shouldn't invent one.

use std::collections::BTreeSet;

use std::collections::BTreeMap;

use comline_core::schema::ir::frozen::unit::FrozenUnit;
use lsp_types::SymbolKind;

use crate::analysis::imports::ResolvedUse;
use crate::analysis::project::{named_type_sites, Project, ProjectDoc};

/// What [`check`] found for one file.
pub struct ImportCheck {
    /// Imports core's per-file lowering can't produce on its own, to append
    /// to the file's units before validation.
    pub scope: Vec<FrozenUnit>,
    /// Each use of a bare type name that's declared in another open file
    /// but not brought into scope by any `use` here.
    pub missing: Vec<MissingImport>,
}

/// One occurrence of a type name that needs a `use`.
pub struct MissingImport {
    pub name: String,
    /// Byte range of this occurrence.
    pub range: (usize, usize),
    /// Every open file declaring `name` as a type, in project order.
    pub candidates: Vec<Candidate>,
    /// The alias a `use` here already imports `name` under, if any - core
    /// binds only the alias (`use types::User as U` makes `U` usable, not
    /// `User`).
    pub imported_as: Option<String>,
}

/// An open file a missing name could be imported from.
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

/// Where one `use` points, as far as the open files can tell.
pub(crate) enum Target {
    /// An open file (index into `Project::docs`), with the rest of the path
    /// past its namespace (`["User"]` for `use types::User`, empty for
    /// `use types`, `use types::*` or `use types::{A, B}`).
    Open(usize, Vec<String>),
    /// No open file has the namespace (or any prefix of it) - closed,
    /// `std::`, or another package.
    NotOpen,
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
            Target::Open(sibling, remaining) => {
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
            Target::NotOpen => {
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

    ImportCheck { scope, missing }
}

/// The open file a `use` points at: the longest prefix of its namespace
/// that some open file other than `exclude` (the file the `use` is in) has.
/// A deeper file always wins: `use chat::admin::X` means `chat/admin.ids`
/// when it's open, not `chat.ids`. The same search as
/// [`crate::analysis::imports::resolve_symbol`].
pub(crate) fn target_of(project: &Project, exclude: Option<usize>, use_decl: &ResolvedUse) -> Target {
    let namespace = &use_decl.resolved.absolute_namespace;

    for split_at in (1..=namespace.len()).rev() {
        let prefix = &namespace[..split_at];
        if let Some(i) = (0..project.docs.len()).find(|&i| Some(i) != exclude && project.docs[i].namespace == prefix) {
            return Target::Open(i, namespace[split_at..].to_vec());
        }
    }

    Target::NotOpen
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
        let project = Project::new(files.iter().map(|(u, s)| (u, s.as_str())));
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
    fn an_alias_binds_only_the_alias() {
        let aliased = "use types::Message as Msg\n\nstruct S {\n    a: Msg\n    b: Message\n}\n";
        let files = project(&[("file:///pkg/src/chat.ids", aliased), TYPES]);
        let project = Project::new(files.iter().map(|(u, s)| (u, s.as_str())));

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
    fn a_glob_or_whole_namespace_use_of_a_file_that_isnt_open_gets_the_benefit_of_the_doubt() {
        for header in ["use other::*", "use other"] {
            let source = format!("{header}\n\nstruct S {{\n    m: Message\n}}\n");
            let files = project(&[("file:///pkg/src/chat.ids", &source), TYPES]);
            assert_eq!(missing_names(&files, 0), vec![], "`Message` may come from `{header}`");
        }
    }

    #[test]
    fn a_glob_of_an_open_file_that_doesnt_declare_the_name_does_not_count() {
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
}
